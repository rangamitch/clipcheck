/**
 * Typed bridge to the recorder daemon, reached through the Tauri backend.
 *
 * Wire types mirror `crates/ipc/src/proto.rs` (protocol v1). That file uses serde
 * snake_case names everywhere, and enums are `{ "type": ..., "data": ... }`. Unit
 * variants carry no `data` key. Outside Tauri (vitest, `vite preview`) every call
 * fails soft: requests reject with `DaemonUnavailableError`, listeners are no-ops.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { isTauriRuntime } from './window';

// ---------------------------------------------------------------------------
// Shared value types
// ---------------------------------------------------------------------------

export type RecState =
  | 'idle'
  | 'armed'
  | 'recording'
  | 'buffer_only'
  | 'splitting'
  | 'stopping'
  | 'paused'
  | 'degraded';

export type Topic = 'state' | 'clips' | 'sessions' | 'health' | 'levels' | 'warnings' | 'settings' | 'meter' | 'library';

export type ErrorCode =
  | 'proto_mismatch'
  | 'unauthorized'
  | 'bad_request'
  | 'unknown_type'
  | 'not_implemented'
  | 'frame_too_large'
  | 'not_recording'
  | 'invalid_state'
  | 'busy'
  | 'shutting_down'
  | 'worker_unavailable'
  | 'internal';

export type AutostartState = 'enabled' | 'disabled' | 'disabled_by_windows' | 'path_stale';

export interface GameRef {
  game_id: string;
  exe: string;
  pid: number;
  title: string | null;
}

export interface GameInfo {
  name: string;
  exe: string;
  hwnd: number;
  attached_since_unix_ms: number;
}

export interface SessionInfo {
  session_id: string;
  part: number;
  started_unix_ms: number;
  path: string;
  parts_saved: number;
}

export interface DiagnosticDto {
  code: string;
  severity: 'error' | 'warning';
  path: string;
  message: string;
}

export interface ConfigIssue {
  key: string;
  message: string;
}

export interface TrackLevel {
  track: number;
  peak_db: number;
  rms_db: number;
}

/** Full status snapshot (`Reply::Status`). */
export interface StatusDto {
  state: RecState;
  game: GameRef | null;
  session: SessionInfo | null;
  buffer_seconds: number;
  buffer_bytes: number;
  encoder: string | null;
  codec: string | null;
  fps_target: number;
  bitrate_kbps: number | null;
  mic_muted: boolean;
  detection_paused: boolean;
  autostart: AutostartState;
  worker_pid: number | null;
  worker_restarts_10min: number;
  uptime_s: number;
  degraded_reason: string | null;
  /** Attached game with display name and attach time, or `null` when none is attached. */
  game_info: GameInfo | null;
  /** Seconds of footage with fractions. */
  buffer_secs: number;
  /** True when a settings change waits until no game is attached. */
  settings_pending_restart: boolean;
  /** Daemon clock in Unix ms at snapshot time. */
  server_unix_ms: number;
  /** False while the recorder is turned off. Missing from older daemons, which means true. */
  recorder_enabled?: boolean;
  /** True while the global hotkeys are paused by a `set_hotkeys_suspended` request. Missing from older daemons. */
  hotkeys_suspended?: boolean;
}

// ---------------------------------------------------------------------------
// Commands (client -> daemon)
// ---------------------------------------------------------------------------

export type Command =
  | { type: 'hello'; data: { proto_min: number; proto_max: number; client: string; client_version: string } }
  | { type: 'ping' }
  | { type: 'get_status' }
  | { type: 'start_session' }
  | { type: 'stop_session' }
  | { type: 'split_session' }
  | { type: 'clip_now'; data: { seconds: number } }
  | { type: 'get_config' }
  | { type: 'patch_config'; data: { patch: Record<string, unknown>; dry_run: boolean } }
  | { type: 'reload_config' }
  | { type: 'rename_recording'; data: { recording_id: string; name: string } }
  | { type: 'set_favorite'; data: { recording_id: string; favorite: boolean } }
  | { type: 'delete_recording'; data: { recording_id: string } }
  | { type: 'register_edited_recording'; data: { source_recording_id: string | null; path: string } }
  | { type: 'subscribe'; data: { topics: Topic[] } }
  | { type: 'unsubscribe'; data: { topics: Topic[] } }
  | { type: 'list_audio_apps' }
  | { type: 'list_audio_devices' }
  | { type: 'list_encoders' }
  | { type: 'list_running_apps' }
  | { type: 'mic_meter'; data: { enable: boolean; device?: string | null } }
  | { type: 'mic_test'; data: { device?: string | null } }
  | { type: 'set_recorder_enabled'; data: { enabled: boolean } }
  | { type: 'set_hotkeys_suspended'; data: { suspended: boolean } }
  | { type: 'shutdown' };

// Discovery wire shapes (flat `{ type, ...fields }`, snake_case). Only
// `discovery.ts` reads these; it maps them to camelCase UI types.

export interface WireAudioApp {
  exe: string;
  name: string;
  icon_png_b64?: string | null;
  active: boolean;
  peak: number;
  last_active_unix_ms: number;
}

export interface WireAudioDevice {
  id: string;
  name: string;
  is_default: boolean;
}

export interface WireGpu {
  name: string;
  vendor: string;
  index: number;
  dedicated_mb: number;
}

export interface WireEncoder {
  backend: string;
  codec: string;
  encoder_name?: string;
  /** Older daemons (pre-D63). */
  ffmpeg_name?: string;
  works: boolean;
}

export interface WireRunningApp {
  exe: string;
  name: string;
  icon_png_b64?: string | null;
  pid: number;
  is_game: boolean;
  in_allow_list: boolean;
  suggested: boolean;
}

// ---------------------------------------------------------------------------
// Replies (daemon -> client, answering one request)
// ---------------------------------------------------------------------------

export type Reply =
  | { type: 'hello_ack'; data: { proto: number; daemon_version: string; daemon_pid: number; features: string[] } }
  | { type: 'pong'; data: { unix_ms: number } }
  | { type: 'status'; data: StatusDto }
  | { type: 'ok' }
  | { type: 'shutdown_ack' }
  | { type: 'clip_queued'; data: { clip_id: string } }
  | { type: 'config'; data: { config: Record<string, unknown>; config_version: number; path: string } }
  | { type: 'config_applied'; data: { apply_class: string; restart_required: boolean; diagnostics: DiagnosticDto[] } }
  | { type: 'config_checked'; data: { apply_class: string; restart_required: boolean; diagnostics: DiagnosticDto[] } }
  | { type: 'config_rejected'; data: { diagnostics: DiagnosticDto[] } }
  | { type: 'recording_registered'; data: { recording_id: string } }
  | { type: 'subscribed'; data: { topics: Topic[] } }
  | { type: 'unsubscribed'; data: { topics: Topic[] } }
  | { type: 'audio_apps'; data: { apps: WireAudioApp[] } }
  | { type: 'audio_devices'; data: { capture: WireAudioDevice[]; render: WireAudioDevice[] } }
  | { type: 'encoders'; data: { encoder_ready?: boolean; /** Older daemons (pre-D63). */ ffmpeg_ready?: boolean; gpus: WireGpu[]; encoders: WireEncoder[] } }
  | { type: 'running_apps'; data: { apps: WireRunningApp[] } }
  | { type: 'mic_test'; data: { wav_base64: string; duration_ms: number; peak_db: number } }
  | { type: 'error'; data: { code: ErrorCode; message: string; retryable: boolean } };

// ---------------------------------------------------------------------------
// Events (daemon -> client, pushed after a matching subscribe)
// ---------------------------------------------------------------------------

export type Event =
  | { type: 'state_changed'; data: { state: RecState; game: GameRef | null } }
  | { type: 'game_detected'; data: { game: GameRef } }
  | { type: 'game_lost'; data: { game_id: string; reason: string } }
  | { type: 'session_started'; data: { session: SessionInfo } }
  | { type: 'session_part_started'; data: { session_id: string; part: number; path: string } }
  | {
      type: 'session_part_finished';
      data: {
        session_id: string;
        part: number;
        path: string;
        duration_ms: number;
        bytes: number;
        recovered: boolean;
        started_unix_ms: number;
        parts_saved: number;
      };
    }
  | { type: 'clip_saved'; data: { clip_id: string; path: string; duration_ms: number; bytes: number } }
  | { type: 'capture_health'; data: { fps: number; dropped_frames: number; black_frames_active: boolean } }
  | { type: 'encoder_selected'; data: { codec: string; encoder: string } }
  | { type: 'audio_levels'; data: { tracks: TrackLevel[] } }
  | { type: 'mic_mute_changed'; data: { muted: boolean } }
  | { type: 'detection_pause_changed'; data: { paused: boolean } }
  | { type: 'config_changed'; data: { apply_class: string; restart_required: boolean } }
  | { type: 'settings_reloaded'; data: { ok: boolean; errors: ConfigIssue[] } }
  | { type: 'worker_restarted'; data: { attempt: number; reason: string; exit_code: number | null } }
  | { type: 'degraded'; data: { reason: string } }
  | { type: 'warning'; data: { code: string; message: string } }
  | { type: 'library_changed'; data: Record<string, never> }
  /** Progress of a library scan of one root. `phase` is walking, probing, thumbnails, done or offline. */
  | {
      type: 'library_scan';
      data: {
        root_id: number;
        path: string;
        phase: string;
        done: number;
        total: number;
        added: number;
        missing: number;
      };
    }
  | { type: 'shutting_down'; data: { reason: string } }
  /** Live input level, sent while subscribed to `meter` and the mic meter is enabled. */
  | { type: 'mic_level'; data: { rms_db: number; peak_db: number } };

/** Payload of the `daemon-connection` Tauri event. */
export interface ConnectionPayload {
  connected: boolean;
}

/** Topics the status view needs. `levels` is deliberately left out (10 Hz). */
export const STATUS_TOPICS: Topic[] = ['state', 'clips', 'sessions', 'health', 'warnings', 'settings', 'library'];

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/** Thrown when the daemon bridge is not reachable (no Tauri, or the backend call failed). */
export class DaemonUnavailableError extends Error {
  constructor(message = 'Recorder is not reachable') {
    super(message);
    this.name = 'DaemonUnavailableError';
  }
}

/**
 * Alias of the shared `isTauriRuntime` helper in `window.ts`, kept so existing imports of
 * `isTauri` keep working. New code should import `isTauriRuntime` directly.
 */
export const isTauri = isTauriRuntime;

/** Narrows a reply to the `error` variant. */
export function isErrorReply(reply: Reply): reply is Extract<Reply, { type: 'error' }> {
  return reply.type === 'error';
}

/** Sends one command and resolves with the daemon's reply (error replies included). */
export async function request(command: Command): Promise<Reply> {
  if (!isTauriRuntime()) throw new DaemonUnavailableError('Not running inside the ClipCheck app');
  try {
    return await invoke<Reply>('daemon_request', { command });
  } catch (e) {
    throw new DaemonUnavailableError(e instanceof Error ? e.message : String(e));
  }
}

/** Whether the backend currently holds a live connection to the daemon. */
export async function daemonConnected(): Promise<boolean> {
  if (!isTauriRuntime()) return false;
  try {
    return (await invoke<boolean>('daemon_connected')) === true;
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------------
// Typed command wrappers
// ---------------------------------------------------------------------------

export function getStatus(): Promise<Reply> {
  return request({ type: 'get_status' });
}

export function startSession(): Promise<Reply> {
  return request({ type: 'start_session' });
}

export function stopSession(): Promise<Reply> {
  return request({ type: 'stop_session' });
}

export function splitSession(): Promise<Reply> {
  return request({ type: 'split_session' });
}

/**
 * Turns the recorder on or off. Turning it off saves a running session up to that point. The
 * reply is `ok` or an error; the caller refreshes the status to read the new state.
 */
export function setRecorderEnabled(enabled: boolean): Promise<Reply> {
  return request({ type: 'set_recorder_enabled', data: { enabled } });
}

/**
 * Pauses (`true`) or resumes (`false`) the daemon's global hotkeys. The daemon resumes on its own
 * 60 s after the last `true` and when the client disconnects, so a holder must keep sending `true`.
 * Use `beginHotkeyCapture` from `settings/hotkeySuspend.ts` rather than calling this directly.
 */
export function setHotkeysSuspended(suspended: boolean): Promise<Reply> {
  return request({ type: 'set_hotkeys_suspended', data: { suspended } });
}

/** Saves the last `seconds` of the replay buffer (valid range 1..=3600). */
export function clipNow(seconds: number): Promise<Reply> {
  return request({ type: 'clip_now', data: { seconds } });
}

export function getConfig(): Promise<Reply> {
  return request({ type: 'get_config' });
}

/** Applies a partial config. With `dryRun` nothing is written. */
export function patchConfig(
  patch: Record<string, unknown>,
  options: { dryRun?: boolean } = {},
): Promise<Reply> {
  return request({ type: 'patch_config', data: { patch, dry_run: options.dryRun === true } });
}

export function subscribe(topics: Topic[]): Promise<Reply> {
  return request({ type: 'subscribe', data: { topics } });
}

export function unsubscribe(topics: Topic[]): Promise<Reply> {
  return request({ type: 'unsubscribe', data: { topics } });
}

// ---------------------------------------------------------------------------
// Listeners
// ---------------------------------------------------------------------------

function listenTo<T>(name: string, handler: (payload: T) => void): () => void {
  if (!isTauriRuntime()) return () => {};
  let disposed = false;
  let unlisten: UnlistenFn | undefined;
  void listen<T>(name, (e) => {
    if (!disposed) handler(e.payload);
  })
    .then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    })
    .catch(() => {
      // Without the listener the view falls back to its periodic poll.
    });
  return () => {
    disposed = true;
    unlisten?.();
  };
}

/** Subscribes to `daemon-event`. Returns an unsubscribe function. */
export function onEvent(handler: (event: Event) => void): () => void {
  return listenTo<Event>('daemon-event', handler);
}

/** Subscribes to `daemon-connection`. Returns an unsubscribe function. */
export function onConnection(handler: (connected: boolean) => void): () => void {
  // `p` can be missing when the backend emits an empty payload, so the optional read stays.
  return listenTo<ConnectionPayload>('daemon-connection', (p) => handler(p?.connected === true));
}
