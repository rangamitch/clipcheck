/**
 * Settings state for the Settings screen: the saved config, the draft being edited, dry-run
 * validation, saving, and the external-change flow. Wire calls go through `daemon.ts`.
 *
 * Model: `saved` is the daemon's view as last seen. `draft` is what the user sees. Every
 * save sends `diffPatch(saved, draft)`, so only changed leaves are written.
 */
import { get, writable, type Readable } from 'svelte/store';
import { getConfig, onEvent, patchConfig, type DiagnosticDto, type Reply } from '../daemon';
import { debounce } from '../debounce';
import { fetchConfigShared, liveStatus } from '../status';
import { createAutosave } from './autosave';
import {
  applyNotice,
  appliesAfterGameEnds,
  cloneJson,
  deepEqual,
  diffPatch,
  fieldDiagnostic,
  getPath,
  hasBlockingErrors,
  setPath as setPathPure,
  type FieldDiagnostic,
  type PlainObject,
} from './pure';

// ---------------------------------------------------------------------------
// Config shape (the serde JSON of crates/core/src/config/schema.rs)
// ---------------------------------------------------------------------------

export type CloseBehavior = 'minimize_to_tray' | 'exit';
export type QuotaAction = 'warn' | 'delete_oldest_clips' | 'stop_recording';
export type DetectionMode = 'auto' | 'allowlist';
export type AfterManualStop = 'until_next_launch' | 'until_reenabled';

export interface GeneralConfig {
  autostart: boolean;
  start_minimized: boolean;
  tray_badge_recording: boolean;
  locale: string;
  close_behavior: CloseBehavior;
  confirm_exit_while_recording: boolean;
}

export interface StorageConfig {
  root: string;
  per_game_folders: boolean;
  quota_gb: number;
  quota_action: QuotaAction;
  filename_template: string;
  delete_to_recycle_bin: boolean;
}

export interface DetectionConfig {
  mode: DetectionMode;
  allow: string[];
  block: string[];
  auto_record: boolean;
  record_sessions: boolean;
  after_manual_stop: AfterManualStop;
  stop_grace_s: number;
}

export type EncoderChoice = 'auto' | 'nvenc' | 'amf' | 'qsv' | 'software';
export type VideoCodec = 'h264' | 'hevc' | 'av1';
export type QualityPreset = 'low' | 'medium' | 'high' | 'ultra' | 'custom';
export type ResolutionChoice = 'native' | '360p' | '480p' | '720p' | '1080p' | '1440p' | '2160p';
export type FpsChoice = 24 | 30 | 60 | 90 | 120 | 144;
export type EncoderSpeed = 'fastest' | 'balanced' | 'quality';
export type RateControl = 'quality' | 'cbr' | 'vbr';
export type AudioCodec = 'aac' | 'opus';
export type ReplayStorage = 'disk' | 'ram';
export type MuteMode = 'toggle' | 'push_to_talk';

/** `[video]`. Field names and ranges follow crates/core/src/config/schema.rs. */
export interface VideoConfig {
  encoder: EncoderChoice;
  codec: VideoCodec;
  container: 'mp4';
  preset: QualityPreset;
  resolution: ResolutionChoice;
  fps: FpsChoice;
  rate_control: RateControl;
  quality_cq: number;
  bitrate_kbps: number;
  max_bitrate_kbps: number;
  keyframe_interval_s: number;
  cursor: boolean;
  /** Encoding GPU: "auto" or the adapter name from the GPU list. */
  gpu: string;
  encoder_speed: EncoderSpeed;
  b_frames: number;
  no_border_capture: boolean;
}

/** `[audio.game]`: the captured game's audio. */
export interface GameAudioConfig {
  enabled: boolean;
  gain_db: number;
}

/** `[audio.mic]`. `device` is "default" or a capture endpoint id. */
export interface MicAudioConfig {
  enabled: boolean;
  device: string;
  gain_db: number;
  gate_enabled: boolean;
  gate_db: number;
  /** Records push to talk as its own track (needs separate tracks and PTT mode). */
  ptt_separate_track: boolean;
  /** Push to talk only: true = the Mix track records the push-to-talk mic; false = Mix keeps the open mic. */
  ptt_mix_gated: boolean;
  /** Keeps the mic open this many ms after the key is released, 0..=2000. */
  ptt_release_ms: number;
}

/** `[audio.other_apps]`: everything that is not the game or a listed app. */
export interface OtherAppsAudioConfig {
  enabled: boolean;
  gain_db: number;
}

/** One `[[audio.apps]]` entry. `exe` is lowercase and ends in ".exe". */
export interface AudioAppConfig {
  exe: string;
  name: string;
  enabled: boolean;
  gain_db: number;
}

/** `[audio]`. Sources and their track layout follow crates/core/src/config/schema.rs (`Audio`). */
export interface AudioConfig {
  codec: AudioCodec;
  bitrate_kbps: number;
  separate_tracks: boolean;
  output_device: string;
  game: GameAudioConfig;
  mic: MicAudioConfig;
  other_apps: OtherAppsAudioConfig;
  apps: AudioAppConfig[];
}

/** `[replay]`. `location` is a folder that may contain %VARS%. */
export interface ReplayConfig {
  enabled: boolean;
  max_seconds: number;
  storage: ReplayStorage;
  location: string;
  ram_max_mb: number;
}

/**
 * How a clip hotkey fires. `tap` saves on a quick press; `long` saves when the key is held for
 * the long-press threshold (400 ms). A missing value from the daemon means `tap`.
 */
export type ClipPress = 'tap' | 'long';

/** One `[[hotkeys.clips]]` entry. `keys` is a canonical chord string. */
export interface ClipHotkeyConfig {
  name: string;
  keys: string;
  seconds: number;
  press: ClipPress;
}

/**
 * `[hotkeys.session]`. An empty string means unbound. `push_to_talk` holds up to 4 chords (keys or
 * mouse buttons Mouse3-5); the mic records while any one is held. `mute_mic_mode` is legacy: when
 * it is 'push_to_talk', `mute_mic` acts as one more hold-to-talk key.
 */
export interface SessionHotkeysConfig {
  split: string;
  stop_start: string;
  toggle_buffer: string;
  mute_mic: string;
  mute_mic_mode: MuteMode;
  push_to_talk: string[];
}

/** `[hotkeys]` */
export interface HotkeysConfig {
  clips: ClipHotkeyConfig[];
  session: SessionHotkeysConfig;
}

/** The whole config. Sections the Settings tabs do not edit yet are kept as opaque JSON. */
export interface ConfigDoc {
  config_version: number;
  general: GeneralConfig;
  storage: StorageConfig;
  detection: DetectionConfig;
  video?: VideoConfig;
  audio?: AudioConfig;
  replay?: ReplayConfig;
  hotkeys?: HotkeysConfig;
  [section: string]: unknown;
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

export interface SettingsState {
  /** Config as the daemon last reported it (or as the last successful save left it). */
  saved: ConfigDoc | null;
  /** What the user is editing. `null` until the first load. */
  draft: ConfigDoc | null;
  loading: boolean;
  /** A dry run is in flight. */
  checking: boolean;
  /** A real save is in flight. */
  saving: boolean;
  /** Transport or unexpected-reply failure. Null when the last call worked. */
  error: string | null;
  /** Findings for the current draft (from the last dry run or the last rejected save). */
  diagnostics: DiagnosticDto[];
  /** True when `draft` differs from `saved`. */
  dirty: boolean;
  /** Apply class of the pending change, from the last dry run or save. */
  applyClass: string | null;
  /** True when the pending change waits until no game is attached. */
  pendingRestart: boolean;
  /** Result of the last save, such as "Applied". Cleared by the next edit. */
  notice: string | null;
  /** A newer config from the daemon, seen while the draft had unsaved edits. */
  external: ConfigDoc | null;
  /** Why the last autosave failed (transport error or unexpected reply). Null when the last save worked. */
  saveError: string | null;
  /** A change waits for its field to lose focus (typed folder paths). It is not saved before that. */
  held: boolean;
}

export const VALIDATE_MS = 400;
/** Quiet time after the last change before autosave writes. */
export const AUTOSAVE_MS = 700;

/**
 * Paths whose text is typed (`PathField` reports every keystroke). A change made while a text
 * input has focus waits for the field to lose focus; a Browse pick saves on the normal debounce.
 */
const BLUR_COMMIT_PATHS: ReadonlySet<string> = new Set(['storage.root', 'replay.location']);

const REJECTED_NOTICE = 'Not saved. Fix the highlighted fields.';

/**
 * Config paths of the rows on screen right now: each mounted `fieldState` subscriber counts once
 * under its path. A rejection uses this to tell whether a finding has a highlighted field.
 */
const mountedPaths = new Map<string, number>();

/** True when a finding at `findingPath` would show in a mounted row (same match as `fieldDiagnostic`). */
function isOnScreen(findingPath: string): boolean {
  for (const path of mountedPaths.keys()) {
    if (findingPath === path || findingPath.startsWith(`${path}.`) || findingPath.startsWith(`${path}[`)) return true;
  }
  return false;
}

const initialState: SettingsState = {
  saved: null,
  draft: null,
  loading: false,
  checking: false,
  saving: false,
  error: null,
  diagnostics: [],
  dirty: false,
  applyClass: null,
  pendingRestart: false,
  notice: null,
  external: null,
  saveError: null,
  held: false,
};

const state = writable<SettingsState>(cloneJson(initialState));

/** Read-only view of the settings state. */
export const settings: Readable<SettingsState> = { subscribe: state.subscribe };

/** What one settings row shows: whether its value differs from saved, its finding and its chip. */
export interface FieldState {
  changed: boolean;
  /** The change waits until the current game ends. */
  deferred: boolean;
  diag: FieldDiagnostic | null;
}

function sameDiagnostic(a: FieldDiagnostic | null, b: FieldDiagnostic | null): boolean {
  if (a === b) return true;
  if (a === null || b === null) return false;
  return a.code === b.code && a.severity === b.severity && a.message === b.message;
}

/**
 * Per-row view of one config path. Every store write runs this cheap check, but the row is only
 * notified when its own `changed`, `deferred` or finding changes. A slider tick or a keystroke
 * elsewhere therefore does not re-render the other rows.
 */
export function fieldState(path: string): Readable<FieldState> {
  return {
    subscribe(run, invalidate) {
      let last: FieldState | null = null;
      mountedPaths.set(path, (mountedPaths.get(path) ?? 0) + 1);
      const unsubscribe = state.subscribe((s) => {
        const changed =
          s.saved !== null && s.draft !== null && !deepEqual(getPath(s.saved, path), getPath(s.draft, path));
        const next: FieldState = {
          changed,
          deferred: changed && s.pendingRestart && appliesAfterGameEnds(path),
          diag: fieldDiagnostic(path, s.diagnostics),
        };
        if (last && last.changed === next.changed && last.deferred === next.deferred && sameDiagnostic(last.diag, next.diag)) {
          return;
        }
        last = next;
        run(next);
      }, invalidate);
      return () => {
        unsubscribe();
        const left = (mountedPaths.get(path) ?? 1) - 1;
        if (left > 0) mountedPaths.set(path, left);
        else mountedPaths.delete(path);
      };
    },
  };
}

/** Bumped by every operation that makes an in-flight dry run stale. */
let seq = 0;

function update(patch: Partial<SettingsState>): void {
  state.update((s) => ({ ...s, ...patch }));
}

function messageOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function errorOf(reply: Reply): string {
  return reply.type === 'error' ? reply.data.message : 'Unexpected reply from the recorder.';
}

const validator = debounce(() => {
  void validateDraft();
}, VALIDATE_MS);

/** Writes the draft after a quiet period. Runs are serialized (see autosave.ts). */
const autosaver = createAutosave({
  debounceMs: AUTOSAVE_MS,
  run: autosaveOnce,
  onError: (e) => update({ saving: false, saveError: messageOf(e) }),
});

/** Clears the store and the event listener (tests and a full reset). */
export function resetSettings(): void {
  validator.cancel();
  autosaver.cancel();
  seq += 1;
  stopSettingsSync();
  sectionMemo.clear();
  state.set(cloneJson(initialState));
}

// ---------------------------------------------------------------------------
// Load
// ---------------------------------------------------------------------------

/** Reads the config from the daemon and makes it both the saved and the draft copy. */
export async function loadSettings(): Promise<void> {
  update({ loading: true, error: null });
  try {
    const reply = await getConfig();
    if (reply.type === 'config') {
      const doc = cloneJson(reply.data.config as ConfigDoc);
      validator.cancel();
      autosaver.cancel();
      seq += 1;
      update({
        saved: doc,
        draft: cloneJson(doc),
        loading: false,
        error: null,
        diagnostics: [],
        dirty: false,
        applyClass: null,
        pendingRestart: false,
        notice: null,
        external: null,
        checking: false,
        saveError: null,
        held: false,
      });
    } else {
      update({ loading: false, error: errorOf(reply) });
    }
  } catch (e) {
    update({ loading: false, error: messageOf(e) });
  }
}

/** Called when the recorder becomes reachable. Loads once, then looks for outside changes. */
export function onRecorderConnected(): void {
  if (get(state).saved === null) {
    if (!get(state).loading) void loadSettings();
  } else {
    // A draft kept while offline saves again once the recorder is back (not while a banner waits).
    void checkExternal().then(() => {
      if (get(state).dirty) autosaver.schedule();
    });
  }
}

// ---------------------------------------------------------------------------
// Edit
// ---------------------------------------------------------------------------

/**
 * Memo for `configEqual`, one entry per top-level section. The draft and saved copies are
 * immutable (edits replace the path they touch), so a section whose two references are both
 * unchanged since the last check cannot have changed. Only the edited section is compared.
 */
const sectionMemo = new Map<string, { savedRef: unknown; draftRef: unknown; equal: boolean }>();

/** Same result as `deepEqual(saved, draft)`, but re-compares only sections whose references changed. */
function configEqual(saved: ConfigDoc | null, draft: ConfigDoc): boolean {
  if (!saved) return false;
  let equal = true;
  const keys = new Set([...Object.keys(saved), ...Object.keys(draft)]);
  for (const key of keys) {
    const savedRef = saved[key];
    const draftRef = draft[key];
    const memo = sectionMemo.get(key);
    let sectionEqual: boolean;
    if (memo && memo.savedRef === savedRef && memo.draftRef === draftRef) {
      sectionEqual = memo.equal;
    } else {
      sectionEqual = deepEqual(savedRef, draftRef);
      sectionMemo.set(key, { savedRef, draftRef, equal: sectionEqual });
    }
    if (!sectionEqual) equal = false;
  }
  return equal;
}

/** Sets one value in the draft by dotted path (immutable). Schedules a dry run. */
export function setPath(path: string, value: unknown): void {
  const s = get(state);
  if (!s.draft) return;
  const typing =
    BLUR_COMMIT_PATHS.has(path) && typeof document !== 'undefined' && isTextEntry(document.activeElement);
  applyDraft(() => setPathPure(s.draft as PlainObject, path, value) as ConfigDoc, typing);
}

/**
 * Applies several draft edits as one change: one state update and one dry run. Used where
 * one user action touches more than one path, such as adding a game (allow list plus games).
 */
export function updateDraft(change: (draft: ConfigDoc) => ConfigDoc): void {
  applyDraft(change, false);
}

function isTextEntry(el: Element | null): boolean {
  return el instanceof HTMLInputElement || el instanceof HTMLTextAreaElement;
}

/**
 * The one place a draft edit lands. `held` edits wait for `flushAutosave` (a field losing focus);
 * all other edits start the autosave debounce.
 */
function applyDraft(change: (draft: ConfigDoc) => ConfigDoc, held: boolean): void {
  const s = get(state);
  if (!s.draft) return;
  const draft = change(s.draft);
  const clean = configEqual(s.saved, draft);
  // Any edit makes an in-flight dry run stale. A dirty draft gets a fresh run from the debounce.
  seq += 1;
  update({
    draft,
    dirty: !clean,
    notice: null,
    checking: clean ? false : s.checking,
    held: !clean && held,
    ...(clean ? { diagnostics: [], applyClass: null, pendingRestart: false } : {}),
  });
  if (clean) {
    validator.cancel();
    autosaver.cancel();
  } else {
    validator.call();
    if (held) {
      // Drop any debounce armed by an earlier edit, so it cannot save a half-typed path.
      autosaver.cancel();
      autosaver.mark();
    } else {
      autosaver.schedule();
    }
  }
}

/** Throws away the draft and goes back to the saved config. Also drops a waiting autosave. */
export function revert(): void {
  const s = get(state);
  if (!s.saved) return;
  validator.cancel();
  autosaver.cancel();
  seq += 1;
  update({
    draft: cloneJson(s.saved),
    dirty: false,
    checking: false,
    diagnostics: [],
    applyClass: null,
    pendingRestart: false,
    notice: null,
    saveError: null,
    held: false,
  });
}

/** Saves the waiting change now. Called on field blur, tab switch, page hide and Ctrl+S. */
export function flushAutosave(): Promise<void> {
  return autosaver.flush();
}

/** Retry after a failed save: clears the error and saves the draft again. */
export function retryAutosave(): Promise<void> {
  update({ saveError: null });
  autosaver.mark();
  return autosaver.flush();
}

// ---------------------------------------------------------------------------
// Dry run
// ---------------------------------------------------------------------------

/** Dry-runs the current diff. Maps findings to field paths via `diagnostics`. */
export async function validateDraft(): Promise<void> {
  const s = get(state);
  if (!s.saved || !s.draft) return;
  const patch = diffPatch(s.saved, s.draft);
  if (Object.keys(patch).length === 0) {
    update({ checking: false, diagnostics: [], applyClass: null, pendingRestart: false });
    return;
  }
  const mine = ++seq;
  update({ checking: true });
  try {
    const reply = await patchConfig(patch, { dryRun: true });
    if (mine !== seq) return;
    if (reply.type === 'config_checked') {
      update({
        checking: false,
        error: null,
        diagnostics: reply.data.diagnostics,
        applyClass: reply.data.apply_class,
        pendingRestart: reply.data.restart_required,
      });
    } else if (reply.type === 'config_rejected') {
      update({
        checking: false,
        error: null,
        diagnostics: reply.data.diagnostics,
        applyClass: null,
        pendingRestart: false,
      });
    } else {
      update({ checking: false, error: errorOf(reply) });
    }
  } catch (e) {
    if (mine === seq) update({ checking: false, error: messageOf(e) });
  }
}

// ---------------------------------------------------------------------------
// Save
// ---------------------------------------------------------------------------

/**
 * Saves the diff. On success the saved copy follows the daemon's view (it may normalise
 * values). On rejection the draft is kept and the findings are shown. Returns true when
 * the change was applied.
 */
export async function save(): Promise<boolean> {
  return (await saveOnce()).kind === 'applied';
}

/** Outcome of one write to the daemon. */
export type SaveResult =
  | { kind: 'applied' }
  | { kind: 'rejected' }
  | { kind: 'skipped' }
  | { kind: 'failed'; reason: string };

async function saveOnce(): Promise<SaveResult> {
  const s = get(state);
  if (!s.saved || !s.draft || s.saving) return { kind: 'skipped' };
  const patch = diffPatch(s.saved, s.draft);
  if (Object.keys(patch).length === 0) return { kind: 'skipped' };

  const submitted = s.draft;
  validator.cancel();
  const mine = ++seq;
  update({ saving: true, checking: false, error: null, saveError: null });

  try {
    const reply = await patchConfig(patch);
    if (reply.type === 'config_applied') {
      const { restart_required, diagnostics, apply_class } = reply.data;
      let saved = cloneJson(submitted);
      try {
        const fresh = await getConfig();
        if (fresh.type === 'config') saved = cloneJson(fresh.data.config as ConfigDoc);
      } catch {
        // Keep the submitted copy; the next config_changed event reconciles it.
      }
      const cur = get(state);
      const draft = deepEqual(cur.draft, submitted) ? cloneJson(saved) : cur.draft;
      const dirty = !deepEqual(saved, draft);
      update({
        saved,
        draft,
        dirty,
        saving: false,
        external: null,
        notice: applyNotice(restart_required),
        ...(mine === seq ? { diagnostics, applyClass: apply_class, pendingRestart: restart_required } : {}),
      });
      if (dirty) validator.call();
      return { kind: 'applied' };
    }
    if (reply.type === 'config_rejected') {
      const findings = reply.data.diagnostics;
      const errors = findings.filter((d) => d.severity === 'error');
      // No error has a highlighted field: name the first error, since the user cannot see it.
      const unseen = errors.length > 0 && !errors.some((d) => isOnScreen(d.path)) ? errors[0] : null;
      update({
        saving: false,
        diagnostics: findings,
        applyClass: null,
        pendingRestart: false,
        notice: unseen ? `Not saved. ${unseen.message}` : REJECTED_NOTICE,
        // Blocking findings take precedence in the footer; this covers a rejection without one.
        saveError: 'the recorder rejected the change',
      });
      return { kind: 'rejected' };
    }
    const reason = errorOf(reply);
    update({ saving: false, error: reason, saveError: reason });
    return { kind: 'failed', reason };
  } catch (e) {
    const reason = messageOf(e);
    update({ saving: false, error: reason, saveError: reason });
    return { kind: 'failed', reason };
  }
}

/**
 * One autosave attempt. It does nothing (and the draft stays) when:
 * - the recorder is offline (dev-safe guard: autosave never writes while disconnected),
 * - the external-change banner is showing (the user chooses Reload or Keep mine first),
 * - the dry run finds blocking errors (the footer asks for fixes).
 * A rejected or failed save keeps the draft; a failure is shown with Retry and is not retried by itself.
 */
async function autosaveOnce(): Promise<void> {
  update({ held: false });
  const s = get(state);
  if (!s.saved || !s.draft || !s.dirty || s.external) return;
  if (!get(liveStatus).connected) return;
  await validateDraft();
  const cur = get(state);
  if (!cur.dirty || cur.external || hasBlockingErrors(cur.diagnostics)) return;
  await saveOnce();
}

// ---------------------------------------------------------------------------
// External changes
// ---------------------------------------------------------------------------

/**
 * Compares the daemon's config with `saved`. With a clean draft the new config is adopted
 * silently. With a dirty draft it is held as `external` for the banner. Our own saves
 * compare equal to `saved` and are ignored.
 */
export async function checkExternal(): Promise<void> {
  const before = get(state);
  if (!before.saved || before.saving) return;
  let fresh: ConfigDoc;
  try {
    const reply = await fetchConfigShared();
    if (reply.type !== 'config') return;
    fresh = cloneJson(reply.data.config as ConfigDoc);
  } catch {
    return;
  }
  const cur = get(state);
  if (!cur.saved || deepEqual(fresh, cur.saved)) return;
  if (!cur.dirty) {
    validator.cancel();
    seq += 1;
    update({
      saved: fresh,
      draft: cloneJson(fresh),
      dirty: false,
      diagnostics: [],
      applyClass: null,
      pendingRestart: false,
      notice: null,
      external: null,
    });
  } else {
    update({ external: fresh });
  }
}

/** Banner "Reload": drops the draft and adopts the outside config. */
export function acceptExternal(): void {
  const s = get(state);
  if (!s.external) return;
  validator.cancel();
  autosaver.cancel();
  seq += 1;
  update({
    saved: cloneJson(s.external),
    draft: cloneJson(s.external),
    dirty: false,
    diagnostics: [],
    applyClass: null,
    pendingRestart: false,
    notice: null,
    external: null,
    saveError: null,
    held: false,
  });
}

/**
 * Banner "Keep mine": keeps the draft. The baseline moves to the daemon's config, so the
 * next save sends exactly what the user changed. Autosave resumes for that change.
 */
export function keepMine(): void {
  const s = get(state);
  if (!s.external) return;
  const dirty = !deepEqual(s.external, s.draft);
  update({
    saved: cloneJson(s.external),
    dirty,
    external: null,
  });
  validator.call();
  if (dirty) autosaver.schedule();
}

let stopSync: (() => void) | null = null;

/**
 * Listens for `config_changed` and `settings_reloaded` events. Idempotent. The listener
 * stays on for the life of the window, so edits in progress survive leaving the tab.
 */
export function startSettingsSync(): void {
  if (stopSync) return;
  stopSync = onEvent((ev) => {
    if (ev.type === 'config_changed' || ev.type === 'settings_reloaded') {
      void checkExternal();
    }
  });
}

/** Drops the event listener started by `startSettingsSync`. */
export function stopSettingsSync(): void {
  stopSync?.();
  stopSync = null;
}
