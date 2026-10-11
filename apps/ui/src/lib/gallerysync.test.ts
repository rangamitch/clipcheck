import { describe, expect, it } from 'vitest';
import { STATUS_TOPICS, type Event } from './daemon';
import { refreshesGallery } from './gallerysync';

describe('gallery refresh triggers', () => {
  it('library_changed refreshes the gallery (files found by a scan)', () => {
    expect(refreshesGallery({ type: 'library_changed', data: {} })).toBe(true);
  });

  it('clip_saved refreshes the gallery', () => {
    const ev: Event = { type: 'clip_saved', data: { clip_id: 'c', path: 'p', duration_ms: 1, bytes: 1 } };
    expect(refreshesGallery(ev)).toBe(true);
  });

  it('unrelated events do not', () => {
    expect(refreshesGallery({ type: 'mic_mute_changed', data: { muted: true } })).toBe(false);
  });

  it('the status subscription includes the library topic that carries library_changed', () => {
    expect(STATUS_TOPICS).toContain('library');
  });
});
