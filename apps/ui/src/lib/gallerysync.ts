import type { Event } from './daemon';

/**
 * True for a `library_scan` event that finished one root (`phase === 'done'`). The daemon sends
 * it after the root's last `library_changed`, so it also covers a scan that changed nothing.
 */
export function isLibraryScanDone(ev: Event): boolean {
  return ev.type === 'library_scan' && ev.data.phase === 'done';
}

/**
 * True for the daemon events after which the gallery listing is stale: a clip or session part
 * was written, the library changed (a scan registered files that were already on disk, or
 * marked some missing), or a library scan of one root finished.
 */
export function refreshesGallery(ev: Event): boolean {
  return (
    ev.type === 'clip_saved' ||
    ev.type === 'session_part_finished' ||
    ev.type === 'library_changed' ||
    isLibraryScanDone(ev)
  );
}
