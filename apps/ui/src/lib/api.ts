/**
 * Typed wrappers around Tauri `invoke`. Field names mirror the serde (camelCase) DTOs in
 * src-tauri/src/commands.rs. The webview never touches the database directly.
 */
import { invoke } from '@tauri-apps/api/core';
import { isTauriRuntime } from './window';

export type RecordingKind = 'clip' | 'session';
export type RecordingStatus = 'pending' | 'ready';

/** Gallery sort choices, as sent to the backend. */
export type SortKey =
  | 'newest'
  | 'oldest'
  | 'name_asc'
  | 'name_desc'
  | 'largest'
  | 'smallest'
  | 'longest';

/** Request body for `list_recordings` (`ListQueryDto` in Rust). */
export interface ListQueryDto {
  text: string | null;
  game: string | null;
  kind: RecordingKind | null;
  favoritesOnly: boolean;
  sort: SortKey;
  limit: number;
  cursor: string | null;
}

/** One recording as shown in the Gallery (`RecordingDto` in Rust). */
export interface RecordingDto {
  id: string;
  kind: RecordingKind;
  sessionId: string | null;
  partIndex: number | null;
  gameId: string | null;
  gameName: string | null;
  fileName: string;
  ext: string;
  sizeBytes: number;
  durationMs: number;
  createdAtMs: number;
  status: RecordingStatus;
  isFavorite: boolean;
  recovered: boolean;
  degraded: boolean;
  needsFaststart: boolean;
}

/** One page of results (`PageDto` in Rust). */
export interface PageDto {
  items: RecordingDto[];
  nextCursor: string | null;
}

/** Aggregate numbers for the Gallery header (`StatsDto` in Rust). */
export interface StatsDto {
  recordings: number;
  clips: number;
  sessions: number;
  totalSizeBytes: number;
  totalDurationMs: number;
}

/** Filter state as the UI holds it. */
export interface GalleryFilters {
  text: string;
  game: string | null;
  kind: RecordingKind | null;
  favoritesOnly: boolean;
  sort: SortKey;
}

export const DEFAULT_PAGE_SIZE = 60;

/** Builds the backend request for a filter state. Empty text becomes `null`. */
export function buildListQuery(
  filters: GalleryFilters,
  cursor: string | null = null,
  limit: number = DEFAULT_PAGE_SIZE,
): ListQueryDto {
  const text = filters.text.trim();
  return {
    text: text.length > 0 ? text : null,
    game: filters.game,
    kind: filters.kind,
    favoritesOnly: filters.favoritesOnly,
    sort: filters.sort,
    limit,
    cursor,
  };
}

/**
 * Returns a backend page unchanged. The Rust DTOs always serialize every field (`null` when
 * absent, no `skip_serializing_if`), so the wire shape is trusted and nothing is copied.
 */
export function mapPage(raw: PageDto): PageDto {
  return raw;
}

/** Lists one page of recordings. */
export async function listRecordings(query: ListQueryDto): Promise<PageDto> {
  const raw = await invoke<PageDto>('list_recordings', { query });
  return mapPage(raw);
}

/** Fetches library-wide totals. */
export function getLibraryStats(): Promise<StatsDto> {
  return invoke<StatsDto>('get_library_stats');
}

/** Encodes bytes as base64 (no data: prefix). Pure; empty input gives ''. */
export function bytesToBase64(bytes: number[]): string {
  let binary = '';
  const CHUNK = 0x8000;
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode(...bytes.slice(i, i + CHUNK));
  }
  return btoa(binary);
}

/**
 * Cached game icons for the given game ids, as base64 PNG (no data: prefix; see `iconSrc`).
 * Ids without an icon are absent from the result. Any error yields {}.
 */
export async function getGameIcons(gameIds: string[]): Promise<Record<string, string>> {
  if (gameIds.length === 0) return {};
  try {
    const raw = await invoke<Record<string, number[]>>('get_game_icons', { gameIds });
    const out: Record<string, string> = {};
    for (const [id, bytes] of Object.entries(raw)) {
      out[id] = bytesToBase64(bytes);
    }
    return out;
  } catch {
    return {};
  }
}

/** One audio track of a recording (`AudioTrackDto` in Rust). `url` is the sidecar, when extracted. */
export interface AudioTrackDto {
  index: number;
  title: string;
  codec: string;
  channels: number;
  sampleRate: number;
  url: string | null;
}

/** What the player needs to open a recording (`PlayerOpenDto` in Rust). */
export interface PlayerOpenDto {
  id: string;
  videoUrl: string;
  durationMs: number;
  hasVideo: boolean;
  playable: boolean;
  width: number;
  height: number;
  videoCodec: string;
  tracks: AudioTrackDto[];
  mixed: boolean;
  audioError: string | null;
  /** True when 2+ audio tracks exist and their sidecars are not prepared yet (D60). */
  audioPending: boolean;
}

/** Probes a recording (fast; sidecars come from prepareAudio, D60). */
export function openPlayer(id: string): Promise<PlayerOpenDto> {
  return invoke<PlayerOpenDto>('open_player', { id });
}

/** Result of `prepare_audio`: the sidecar-backed track list (`AudioPrepDto` in Rust). */
export interface AudioPrepDto {
  tracks: AudioTrackDto[];
  mixed: boolean;
  audioError: string | null;
}

/**
 * Extracts (or reuses) the per-track audio sidecars. Slow on the first open of a multi-track
 * recording, so the player calls it in the background after playback has started (D60).
 */
export function prepareAudio(id: string): Promise<AudioPrepDto> {
  return invoke<AudioPrepDto>('prepare_audio', { id });
}

/** Details only the backend knows (`RecordingDetailsDto` in Rust). */
export interface RecordingDetailsDto {
  id: string;
  path: string;
  dir: string;
  fileName: string;
  sizeBytes: number;
  exists: boolean;
}

/** Full file location and on-disk facts for the details panel. */
export function getRecordingDetails(id: string): Promise<RecordingDetailsDto> {
  return invoke<RecordingDetailsDto>('get_recording_details', { id });
}

/** Loopback media server base (`http://127.0.0.1:{port}/{token}`), or null when unavailable. */
let mediaBase: Promise<string | null> | null = null;

/**
 * Resolves the native streaming server base once and caches the promise. Outside Tauri, or on any
 * error, it resolves to null and callers use the clip.localhost fallback (see `mediaUrl`).
 */
export function getMediaBase(): Promise<string | null> {
  if (mediaBase === null) {
    mediaBase = (async (): Promise<string | null> => {
      if (!isTauriRuntime()) return null;
      try {
        const base = await invoke<string | null>('media_base_url');
        return typeof base === 'string' && base !== '' ? base : null;
      } catch {
        return null;
      }
    })();
  }
  return mediaBase;
}

/** Origin of the clip media protocol, used when no loopback base exists. */
const CLIP_MEDIA_ORIGIN = 'http://clip.localhost';

/**
 * URL of a recording's video (`rec`) or one audio sidecar (`aud`, needs `track`). Uses the
 * loopback base when given, otherwise the clip.localhost fallback.
 */
export function mediaUrl(
  base: string | null,
  kind: 'rec' | 'aud',
  id: string,
  track: number = 0,
): string {
  const root = base !== null ? base.replace(/\/+$/, '') : CLIP_MEDIA_ORIGIN;
  const path =
    kind === 'rec'
      ? `/rec/${encodeURIComponent(id)}`
      : `/aud/${encodeURIComponent(id)}/${track}`;
  return root + path;
}

// Resolve early so the base is ready before the first clip opens. Never rejects.
void getMediaBase();
