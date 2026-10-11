// Poster URLs for gallery cards. The backend serves `/thumb/<id>` next to `/rec/<id>` on the
// clip media origin (player.rs builds `http://clip.localhost/rec/{id}` the same way). A 404
// means the poster has not been written yet; callers fall back to a placeholder.

/** Origin of the clip media protocol as WebView2 exposes it on Windows. */
const CLIP_ORIGIN = 'http://clip.localhost';

/** URL of the poster image for a recording id. */
export function thumbUrl(id: string): string {
  return `${CLIP_ORIGIN}/thumb/${encodeURIComponent(id)}`;
}
