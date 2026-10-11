/**
 * Live recorder status for the title-bar chip and the Gallery live panel.
 *
 * Pure reducers (`reduceEvent`, `applySnapshot`, `reduceConnection`) hold all the logic
 * and are unit-tested. The stores and `startLiveStatus` wire them to the daemon bridge.
 */
import { get, readable, writable } from 'svelte/store';
import { formatClock } from './format';
import {
  STATUS_TOPICS,
  daemonConnected,
  getConfig,
  getStatus,
  onConnection,
  onEvent,
  subscribe,
  type Event,
  type RecState,
  type Reply,
  type StatusDto,
} from './daemon';

/** Re-exported from `format.ts`, where the clock formatter now lives. */
export { formatClock };

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

export interface LiveGame {
  /** Display name: the config override when known, else the exe name without `.exe`. */
  name: string;
  exe: string;
  /** Attach time in server Unix ms. */
  sinceMs: number;
  /** Stable daemon id, used to match `game_lost`. */
  gameId: string;
}

export interface LiveSession {
  id: string;
  part: number;
  partsSaved: number;
  /** Start of the whole session in server Unix ms. Never moves when the session splits. */
  startedMs: number;
}

export interface LiveWarning {
  code: string;
  message: string;
}

export interface LiveStatus {
  connected: boolean;
  /** Recording state, or `null` while offline. */
  state: RecState | null;
  /** False while the recorder is turned off. Kept at its last value while offline. */
  recorderEnabled: boolean;
  game: LiveGame | null;
  session: LiveSession | null;
  bufferSecs: number;
  encoder: string | null;
  settingsPendingRestart: boolean;
  /** Server clock minus local clock, in ms. Add it to local time to get server time. */
  serverOffsetMs: number;
  degradedReason: string | null;
  warning: LiveWarning | null;
}

/** Config values the panel needs. Empty until `get_config` succeeds. */
export interface LiveConfig {
  maxBufferSecs: number | null;
  clipPresets: { name: string; seconds: number }[];
}

export const INITIAL_STATUS: LiveStatus = {
  connected: false,
  state: null,
  recorderEnabled: true,
  game: null,
  session: null,
  bufferSecs: 0,
  encoder: null,
  settingsPendingRestart: false,
  serverOffsetMs: 0,
  degradedReason: null,
  warning: null,
};

export const INITIAL_CONFIG: LiveConfig = { maxBufferSecs: null, clipPresets: [] };

/** A transient notice, such as a saved clip. */
export interface Toast {
  id: number;
  text: string;
}

export const TOAST_MS = 4000;

// ---------------------------------------------------------------------------
// Clock helpers
// ---------------------------------------------------------------------------

/**
 * Elapsed milliseconds since `startedMs`, measured on the server clock.
 * `nowMs` is local time; `offsetMs` is server minus local. Never negative.
 */
export function elapsedMs(startedMs: number, nowMs: number, offsetMs: number): number {
  const elapsed = nowMs + offsetMs - startedMs;
  return Number.isFinite(elapsed) && elapsed > 0 ? elapsed : 0;
}

/** Ticking local time in ms, updated once a second. Paused while the document is hidden. */
export const now = readable<number>(Date.now(), (set) => {
  let timer: ReturnType<typeof setInterval> | undefined;
  const hasDocument = typeof document !== 'undefined';

  const start = (): void => {
    if (timer !== undefined) return;
    set(Date.now());
    timer = setInterval(() => set(Date.now()), 1000);
  };
  const stop = (): void => {
    if (timer === undefined) return;
    clearInterval(timer);
    timer = undefined;
  };
  const onVisibility = (): void => {
    if (document.hidden) stop();
    else start();
  };

  if (!hasDocument || !document.hidden) start();
  if (hasDocument) document.addEventListener('visibilitychange', onVisibility);

  return () => {
    stop();
    if (hasDocument) document.removeEventListener('visibilitychange', onVisibility);
  };
});

// ---------------------------------------------------------------------------
// Pure reducers
// ---------------------------------------------------------------------------

/** Display name for an exe: the file name without a trailing `.exe`. */
export function exeDisplayName(exe: string): string {
  return exe.replace(/\.exe$/i, '') || exe;
}

function serverNow(localNowMs: number, offsetMs: number): number {
  return localNowMs + offsetMs;
}

function gameFromRef(
  ref: { game_id: string; exe: string },
  sinceMs: number,
  displayName?: string,
): LiveGame {
  return {
    name: displayName ?? exeDisplayName(ref.exe),
    exe: ref.exe,
    sinceMs,
    gameId: ref.game_id,
  };
}

/** Keeps the existing game (and its attach time) when the id matches, else starts a new one. */
function mergeGame(
  current: LiveGame | null,
  ref: { game_id: string; exe: string },
  localNowMs: number,
  offsetMs: number,
): LiveGame {
  if (current && current.gameId === ref.game_id) {
    return { ...current, exe: ref.exe };
  }
  return gameFromRef(ref, serverNow(localNowMs, offsetMs));
}

/**
 * Estimates the clock offset from a snapshot's `server_unix_ms`. Keeps the old value when the
 * daemon sent a non-positive or non-finite clock (it never stamps one that way).
 */
function offsetFrom(serverUnixMs: number, localNowMs: number, previous: number): number {
  if (!Number.isFinite(serverUnixMs) || serverUnixMs <= 0) {
    return previous;
  }
  return serverUnixMs - localNowMs;
}

/**
 * Applies a full `get_status` snapshot. Connection flags are left to `reduceConnection`.
 * Returns `prev` itself when the result would be equal, so subscribers are not notified.
 */
export function applySnapshot(prev: LiveStatus, snap: StatusDto, localNowMs: number): LiveStatus {
  const offsetMs = offsetFrom(snap.server_unix_ms, localNowMs, prev.serverOffsetMs);

  let game: LiveGame | null = null;
  if (snap.game_info) {
    game = {
      // An empty config-override name is a real value the daemon sends; fall back to the exe.
      name: snap.game_info.name || exeDisplayName(snap.game_info.exe),
      exe: snap.game_info.exe,
      sinceMs: snap.game_info.attached_since_unix_ms,
      // `game` is null when only `game_info` is present; the exe then stands in as the id.
      gameId: snap.game?.game_id ?? snap.game_info.exe,
    };
  } else if (snap.game) {
    game = gameFromRef(snap.game, serverNow(localNowMs, offsetMs));
  }

  const session: LiveSession | null = snap.session
    ? {
        id: snap.session.session_id,
        part: snap.session.part,
        partsSaved: snap.session.parts_saved,
        startedMs: snap.session.started_unix_ms,
      }
    : null;

  const next: LiveStatus = {
    ...prev,
    connected: true,
    state: snap.state,
    // A missing field means an older daemon, which always records when it runs.
    recorderEnabled: snap.recorder_enabled !== false,
    game,
    session,
    bufferSecs: snap.buffer_secs,
    encoder: snap.encoder,
    settingsPendingRestart: snap.settings_pending_restart,
    serverOffsetMs: offsetMs,
    degradedReason: snap.degraded_reason,
  };
  return sameStatus(prev, next) ? prev : next;
}

/** Applies a connection change. Going offline clears everything live. */
export function reduceConnection(prev: LiveStatus, connected: boolean): LiveStatus {
  if (connected) return { ...prev, connected: true };
  return {
    ...prev,
    connected: false,
    state: null,
    game: null,
    session: null,
    bufferSecs: 0,
    encoder: null,
    settingsPendingRestart: false,
    degradedReason: null,
  };
}

/**
 * Applies one pushed daemon event. Pure: no toasts, no I/O. Events that do not change the
 * status view (health samples, audio levels, mic and pause flags) return `prev` unchanged.
 */
export function reduceEvent(prev: LiveStatus, ev: Event, localNowMs: number): LiveStatus {
  const off = prev.serverOffsetMs;
  switch (ev.type) {
    case 'state_changed': {
      const game = ev.data.game
        ? mergeGame(prev.game, ev.data.game, localNowMs, off)
        : null;
      const noSession = ev.data.state === 'idle' || ev.data.state === 'buffer_only';
      return {
        ...prev,
        state: ev.data.state,
        game,
        session: noSession ? null : prev.session,
      };
    }
    case 'game_detected':
      return { ...prev, game: mergeGame(prev.game, ev.data.game, localNowMs, off) };
    case 'game_lost': {
      if (!prev.game || prev.game.gameId !== ev.data.game_id) return prev;
      // A pending settings change applies once no game is attached.
      return { ...prev, game: null, settingsPendingRestart: false };
    }
    case 'session_started': {
      const s = ev.data.session;
      return {
        ...prev,
        session: {
          id: s.session_id,
          part: s.part,
          partsSaved: s.parts_saved,
          startedMs: s.started_unix_ms,
        },
      };
    }
    case 'session_part_started': {
      const cur = prev.session;
      // Only the current session moves. A part of an unknown session never starts a clock.
      if (!cur || cur.id !== ev.data.session_id) return prev;
      return { ...prev, session: { ...cur, part: ev.data.part } };
    }
    case 'session_part_finished': {
      const cur = prev.session;
      if (!cur || cur.id !== ev.data.session_id) return prev;
      // startedMs is kept on purpose: the session clock spans every part.
      return { ...prev, session: { ...cur, partsSaved: ev.data.parts_saved } };
    }
    case 'encoder_selected':
      return { ...prev, encoder: ev.data.encoder };
    case 'config_changed':
      return { ...prev, settingsPendingRestart: ev.data.restart_required };
    case 'degraded':
      return { ...prev, state: 'degraded', degradedReason: ev.data.reason };
    case 'warning':
      return { ...prev, warning: { code: ev.data.code, message: ev.data.message } };
    default:
      // No live-panel state: clip_saved (toast only, see applyEvent), capture_health,
      // audio_levels, mic_mute_changed, detection_pause_changed, worker_restarted,
      // shutting_down, settings_reloaded, and the library_* and mic_level events.
      return prev;
  }
}

/** Reads the config fields the panel uses. Tolerates missing or malformed sections. */
export function parseLiveConfig(config: Record<string, unknown>): LiveConfig {
  const replay = config.replay as { max_seconds?: unknown } | undefined;
  const maxSeconds = typeof replay?.max_seconds === 'number' ? replay.max_seconds : null;

  const hotkeys = config.hotkeys as { clips?: unknown } | undefined;
  const clipPresets: { name: string; seconds: number }[] = [];
  if (Array.isArray(hotkeys?.clips)) {
    for (const entry of hotkeys.clips) {
      const e = entry as { name?: unknown; seconds?: unknown };
      if (typeof e?.seconds === 'number' && e.seconds > 0) {
        clipPresets.push({
          name: typeof e.name === 'string' && e.name.length > 0 ? e.name : `${e.seconds}s`,
          seconds: e.seconds,
        });
      }
    }
  }
  return { maxBufferSecs: maxSeconds, clipPresets };
}

// ---------------------------------------------------------------------------
// Chip description (pure, for StatusChip and tests)
// ---------------------------------------------------------------------------

export interface ChipView {
  tone: 'recording' | 'buffering' | 'idle' | 'offline' | 'recorder_off';
  /** Game name, ellipsised by CSS. Null when there is none to show. */
  game: string | null;
  /** Short state label such as `Buffering`, `Idle — waiting for a game`, `Recorder offline`, `Recorder off`. */
  label: string;
  /** Clock text, or null. */
  clock: string | null;
  tooltip: string;
}

/** Describes what the title-bar chip shows for a status and a local time. */
export function describeChip(s: LiveStatus, localNowMs: number): ChipView {
  if (!s.connected) {
    return {
      tone: 'offline',
      game: null,
      label: 'Recorder offline',
      clock: null,
      tooltip: 'The recorder is not running or cannot be reached.',
    };
  }

  if (s.session) {
    const game = s.game?.name ?? null;
    const lines = [
      game ? `Recording ${game}` : 'Recording',
      `Session ${formatClock(elapsedMs(s.session.startedMs, localNowMs, s.serverOffsetMs))} elapsed`,
      `Part ${s.session.part} · ${s.session.partsSaved} saved`,
    ];
    if (s.encoder) lines.push(`Encoder ${s.encoder}`);
    return {
      tone: 'recording',
      game: game ?? 'Recording',
      label: '',
      clock: formatClock(elapsedMs(s.session.startedMs, localNowMs, s.serverOffsetMs)),
      tooltip: lines.join('\n'),
    };
  }

  // The recorder is off: nothing buffers or records. This wins over the waiting-for-game state.
  if (!s.recorderEnabled) {
    return {
      tone: 'recorder_off',
      game: null,
      label: 'Recorder off',
      clock: null,
      tooltip: 'The recorder is off. Turn it on to record again.',
    };
  }

  if (s.game) {
    const since = formatClock(elapsedMs(s.game.sinceMs, localNowMs, s.serverOffsetMs));
    const lines = [
      `Buffering ${s.game.name}`,
      `Attached for ${since}`,
      `Buffer ${formatClock(s.bufferSecs * 1000)}`,
    ];
    if (s.encoder) lines.push(`Encoder ${s.encoder}`);
    return {
      tone: 'buffering',
      game: s.game.name,
      label: 'Buffering',
      clock: since,
      tooltip: lines.join('\n'),
    };
  }

  return {
    tone: 'idle',
    game: null,
    label: 'Idle — waiting for a game',
    clock: null,
    tooltip: 'No supported game is running. The recorder starts when one is detected.',
  };
}

// ---------------------------------------------------------------------------
// Stores
// ---------------------------------------------------------------------------

export const liveStatus = writable<LiveStatus>(INITIAL_STATUS);
export const liveConfig = writable<LiveConfig>(INITIAL_CONFIG);
export const toasts = writable<Toast[]>([]);

let toastSeq = 0;

/** Adds a toast and removes it after `TOAST_MS`. */
export function pushToast(text: string): void {
  toastSeq += 1;
  const id = toastSeq;
  toasts.update((list) => [...list, { id, text }]);
  setTimeout(() => {
    toasts.update((list) => list.filter((t) => t.id !== id));
  }, TOAST_MS);
}

/** Base name of a path, accepting both separators. */
export function baseName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** Offset estimates closer than this are IPC latency noise: the previous offset is kept. */
const OFFSET_NOISE_MS = 1000;

function sameValue(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (a === null || b === null || typeof a !== 'object' || typeof b !== 'object') return false;
  return JSON.stringify(a) === JSON.stringify(b);
}

/**
 * Structural equality for the status object. Small offset drift counts as equal, so a poll
 * that only re-measures the clock does not notify subscribers.
 */
export function sameStatus(a: LiveStatus, b: LiveStatus): boolean {
  const keys = Object.keys(a) as (keyof LiveStatus)[];
  if (keys.length !== Object.keys(b).length) return false;
  for (const k of keys) {
    if (k === 'serverOffsetMs') {
      if (Math.abs(a.serverOffsetMs - b.serverOffsetMs) < OFFSET_NOISE_MS) continue;
      return false;
    }
    if (!sameValue(a[k], b[k])) return false;
  }
  return true;
}

/** Writes the store only when the computed status differs, so no-op events notify nobody. */
function commitStatus(compute: (s: LiveStatus) => LiveStatus): void {
  const prev = get(liveStatus);
  const next = compute(prev);
  if (next === prev || sameStatus(prev, next)) return;
  liveStatus.set(next);
}

/** Applies one event to the stores, including side effects (toasts, config refresh). */
export function applyEvent(ev: Event): void {
  const localNowMs = Date.now();
  commitStatus((s) => reduceEvent(s, ev, localNowMs));
  if (ev.type === 'clip_saved') {
    pushToast(`Saved clip: ${baseName(ev.data.path)}`);
  }
  if (ev.type === 'config_changed' || ev.type === 'settings_reloaded') {
    void refreshConfig();
  }
}

/** Pulls a status snapshot into the store. Failures are ignored; the next poll retries. */
export async function refreshStatus(): Promise<void> {
  try {
    const reply: Reply = await getStatus();
    if (reply.type === 'status') {
      const localNowMs = Date.now();
      commitStatus((s) => applySnapshot(s, reply.data, localNowMs));
    }
  } catch {
    // Connection events report real outages.
  }
}

let configInFlight: Promise<Reply> | null = null;

/**
 * One `get_config` round trip shared by callers that start while it is pending. The config
 * listeners in this module and in `settings/store.ts` both react to the same event, so they
 * share one fetch instead of making two. A call made after the reply starts a new fetch.
 */
export function fetchConfigShared(): Promise<Reply> {
  if (configInFlight === null) {
    configInFlight = getConfig().finally(() => {
      configInFlight = null;
    });
  }
  return configInFlight;
}

/** Pulls the config fields the panel uses. Failures keep the previous values. */
export async function refreshConfig(): Promise<void> {
  try {
    const reply: Reply = await fetchConfigShared();
    if (reply.type === 'config') {
      const parsed = parseLiveConfig(reply.data.config);
      liveConfig.set(parsed);
    }
  } catch {
    // Keep the previous values.
  }
}

/** Handles a connection change: resets live state when offline, refreshes and subscribes when online. */
export function handleConnection(connected: boolean): void {
  commitStatus((s) => reduceConnection(s, connected));
  if (connected) {
    void subscribe(STATUS_TOPICS).catch(() => {});
    void refreshStatus();
    void refreshConfig();
  } else {
    liveConfig.set(INITIAL_CONFIG);
  }
}

/**
 * Poll interval for the safety-net status refresh while connected. Pushed events carry the
 * state, so the poll only corrects drift from missed events. It is deliberately slow.
 */
export const POLL_MS = 30000;

/**
 * Starts the live feed: event and connection listeners, the initial connection check,
 * and the safety-net poll (`POLL_MS`, 30 s) while connected. The poll pauses while the
 * document is hidden and resumes with an immediate refresh when it is shown. Returns a stop function.
 */
export function startLiveStatus(): () => void {
  const unsubEvents = onEvent(applyEvent);
  const unsubConnection = onConnection(handleConnection);
  const hasDocument = typeof document !== 'undefined';
  let poll: ReturnType<typeof setInterval> | undefined;

  const startPoll = (): void => {
    if (poll !== undefined) return;
    poll = setInterval(() => {
      if (get(liveStatus).connected) void refreshStatus();
    }, POLL_MS);
  };
  const stopPoll = (): void => {
    if (poll === undefined) return;
    clearInterval(poll);
    poll = undefined;
  };
  const onVisibility = (): void => {
    if (document.hidden) {
      stopPoll();
      return;
    }
    if (get(liveStatus).connected) void refreshStatus();
    startPoll();
  };

  if (!hasDocument || !document.hidden) startPoll();
  if (hasDocument) document.addEventListener('visibilitychange', onVisibility);

  void daemonConnected().then((connected) => {
    if (connected) handleConnection(true);
  });

  return () => {
    stopPoll();
    if (hasDocument) document.removeEventListener('visibilitychange', onVisibility);
    unsubEvents();
    unsubConnection();
  };
}
