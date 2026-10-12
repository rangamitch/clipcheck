/**
 * Browser-side cache of media previews for the timeline: filmstrip thumbnails and audio peaks.
 *
 * Requests are deduplicated while in flight, results live in a bounded LRU keyed by media id, and
 * every failure is swallowed: a media file whose assets cannot be read simply draws a placeholder.
 * Callers pass an `onchange` callback that is invoked whenever new data is ready, so the timeline
 * can redraw.
 */
import { writable } from 'svelte/store';
import { editorApi, onPeaksReady } from './api';
import type { MediaRef, Project, Tick } from './types';
import { tickToMs } from './ticks';

/** Thumbnail strip of one video media file. Frame i shows the source at ((i + 0.5) / n) of the duration. */
export interface ThumbStrip {
  /** Media duration the strip was made for, in ticks. */
  durationTicks: Tick;
  /** Decoded frames in source order; null where the image is missing or failed to load. */
  frames: (HTMLImageElement | null)[];
}

/**
 * Current audio level per track id, in dB, for the track header meters. Filled by the playback
 * engine through `setTrackLevels`; may be empty. Components subscribe with `$trackLevels`.
 */
export const trackLevels = writable<Record<string, number>>({});

/** Replaces the track level readings shown by the track header meters. */
export function setTrackLevels(levels: Record<string, number>): void {
  trackLevels.set(levels);
}

/** Most media entries kept in memory at once. */
const MAX_MEDIA = 64;
/** Peak buckets requested for a whole media file. */
const PEAK_BUCKETS = 2000;
/** Height of each thumbnail, px. */
const THUMB_HEIGHT = 48;
/** Fewest thumbnails per media file. */
const MIN_THUMBS = 4;
/** Most thumbnails per media file. */
const MAX_THUMBS = 40;

/** Cached state of one media file. Replaced when its duration changes (relink). */
interface MediaEntry {
  durationTicks: Tick;
  thumbs: ThumbStrip | null;
  thumbsBusy: boolean;
  thumbsFailed: boolean;
  peaks: Map<number, Float32Array>;
  peaksBusy: Set<number>;
  /** Streams that failed to load. They are not retried until the entry is replaced. */
  peaksFailed: Set<number>;
  /**
   * Streams the backend has not produced yet (for long media it answers with an empty array and
   * emits `peaks-ready` later). Not retried until that event arrives.
   */
  peaksPending: Set<number>;
}

/** Insertion-ordered map: the first key is the least recently used media. */
const entries = new Map<string, MediaEntry>();

/** Latest onchange callback per peak key, so a `peaks-ready` event can reload the waveform. */
const peakCallbacks = new Map<string, () => void>();

/** True once the `peaks-ready` listener has been requested. */
let listening = false;

/** Key of one audio stream of one media file. */
function peakKey(mediaId: string, stream: number): string {
  return `${mediaId}:${stream}`;
}

/** Finds a media entry of the project, or undefined. */
function findMedia(project: Project, mediaId: string): MediaRef | undefined {
  return project.media.find((m) => m.id === mediaId);
}

/** Moves an existing entry to the most recently used position. */
function touch(mediaId: string): MediaEntry | undefined {
  const entry = entries.get(mediaId);
  if (entry) {
    entries.delete(mediaId);
    entries.set(mediaId, entry);
  }
  return entry;
}

/**
 * Entry for a media file, created when missing. An entry made for a different duration is
 * discarded (the file was relinked). Evicts the least recently used entries beyond `MAX_MEDIA`.
 */
function entryFor(mediaId: string, durationTicks: Tick): MediaEntry {
  const existing = touch(mediaId);
  if (existing && existing.durationTicks === durationTicks) {
    return existing;
  }
  const fresh: MediaEntry = {
    durationTicks,
    thumbs: null,
    thumbsBusy: false,
    thumbsFailed: false,
    peaks: new Map<number, Float32Array>(),
    peaksBusy: new Set<number>(),
    peaksFailed: new Set<number>(),
    peaksPending: new Set<number>(),
  };
  entries.delete(mediaId);
  entries.set(mediaId, fresh);
  while (entries.size > MAX_MEDIA) {
    const oldest = entries.keys().next();
    if (oldest.done) break;
    entries.delete(oldest.value);
  }
  return fresh;
}

/** Creates an image from a data URL that calls `onload` when it has decoded. */
function makeImage(src: string, onload: () => void): HTMLImageElement {
  const img = new Image();
  img.decoding = 'async';
  img.onload = () => onload();
  img.src = src;
  return img;
}

/** Subscribes once to `peaks-ready`. Failures leave the subscription retryable. */
function ensureListening(): void {
  if (listening) return;
  listening = true;
  try {
    onPeaksReady((p) => {
      const entry = touch(p.mediaId);
      if (entry) {
        entry.peaks.delete(p.stream);
        entry.peaksFailed.delete(p.stream);
        entry.peaksPending.delete(p.stream);
      }
      const key = peakKey(p.mediaId, p.stream);
      const cb = peakCallbacks.get(key);
      if (entry && cb) {
        loadPeaks(p.mediaId, p.stream, entry, cb);
      }
    }).catch(() => {
      listening = false;
    });
  } catch {
    listening = false;
  }
}

/**
 * Fetches the peaks of one stream into `entry`. Calls `onchange` when the result is final (data or
 * failure). An empty result marks the stream pending and does NOT call `onchange`, so the timeline
 * does not redraw and re-request in a loop; `peaks-ready` later reloads it. Deduplicated.
 */
function loadPeaks(mediaId: string, stream: number, entry: MediaEntry, onchange: () => void): void {
  if (entry.peaksBusy.has(stream)) return;
  entry.peaksBusy.add(stream);
  let notify = true;
  editorApi
    .waveform(mediaId, stream, PEAK_BUCKETS)
    .then((values) => {
      if (values.length > 0) {
        entry.peaks.set(stream, Float32Array.from(values));
      } else {
        entry.peaksPending.add(stream);
        notify = false;
      }
    })
    .catch(() => {
      entry.peaksFailed.add(stream);
    })
    .finally(() => {
      entry.peaksBusy.delete(stream);
      if (notify) {
        onchange();
      }
    });
}

/** Thumbnail strip of a media file, or null while it is loading or unavailable. */
export function getThumbStrip(mediaId: string): ThumbStrip | null {
  return touch(mediaId)?.thumbs ?? null;
}

/** Peak values (0..=1, `PEAK_BUCKETS` buckets over the whole file) of one audio stream, or null. */
export function getPeaks(mediaId: string, stream: number): Float32Array | null {
  return touch(mediaId)?.peaks.get(stream) ?? null;
}

/**
 * Requests the thumbnail strip of a video media file. Evenly spaced frames across the duration,
 * N = clamp(duration_s / 2, 4, 40), each `THUMB_HEIGHT` px tall. Does nothing for missing media,
 * non-video media, or media already loaded, loading or failed.
 */
export function requestThumbs(project: Project, mediaId: string, onchange: () => void): void {
  const media = findMedia(project, mediaId);
  if (!media || media.missing || media.kind !== 'video') return;
  const durationMs = tickToMs(media.duration);
  if (durationMs <= 0) return;

  const entry = entryFor(mediaId, media.duration);
  if (entry.thumbs !== null || entry.thumbsBusy || entry.thumbsFailed) return;

  const count = Math.min(MAX_THUMBS, Math.max(MIN_THUMBS, Math.round(durationMs / 2000)));
  const timesMs: number[] = [];
  for (let i = 0; i < count; i += 1) {
    timesMs.push(Math.round(((i + 0.5) / count) * durationMs));
  }

  entry.thumbsBusy = true;
  editorApi
    .thumbnails(mediaId, timesMs, THUMB_HEIGHT)
    .then((urls) => {
      const frames: (HTMLImageElement | null)[] = [];
      for (let i = 0; i < count; i += 1) {
        const url = urls[i];
        frames.push(url === undefined || url === '' ? null : makeImage(url, onchange));
      }
      entry.thumbs = { durationTicks: media.duration, frames };
    })
    .catch(() => {
      entry.thumbsFailed = true;
    })
    .finally(() => {
      entry.thumbsBusy = false;
      onchange();
    });
}

/**
 * Requests the peaks of one audio stream of a media file (`PEAK_BUCKETS` buckets over the whole
 * file). A later `peaks-ready` event reloads them. Does nothing for missing media or when the
 * stream is already loaded, loading, pending or failed.
 */
export function requestPeaks(
  project: Project,
  mediaId: string,
  stream: number,
  onchange: () => void,
): void {
  const media = findMedia(project, mediaId);
  if (!media || media.missing || media.duration <= 0) return;
  ensureListening();

  const entry = entryFor(mediaId, media.duration);
  peakCallbacks.set(peakKey(mediaId, stream), onchange);
  if (
    entry.peaks.has(stream) ||
    entry.peaksBusy.has(stream) ||
    entry.peaksPending.has(stream) ||
    entry.peaksFailed.has(stream)
  ) {
    return;
  }
  loadPeaks(mediaId, stream, entry, onchange);
}

/** Drops every cached thumbnail and waveform. Track levels are kept. */
export function clearMediaCache(): void {
  entries.clear();
  peakCallbacks.clear();
}
