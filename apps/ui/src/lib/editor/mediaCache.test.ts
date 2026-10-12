import { beforeEach, describe, expect, it, vi } from 'vitest';
import { editorApi } from './api';
import {
  clearMediaCache,
  getThumbStrip,
  peakBucketsFor,
  requestThumbs,
  thumbCountFor,
} from './mediaCache';
import { TICKS_PER_SECOND } from './ticks';
import type { MediaRef, Project } from './types';

// The backend is replaced by spies. Thumbnail URLs are empty, so no Image objects are created.
vi.mock('./api', () => ({
  editorApi: { thumbnails: vi.fn(), waveform: vi.fn() },
  onPeaksReady: vi.fn(() => Promise.resolve(() => {})),
}));

function videoMedia(id: string, seconds: number): MediaRef {
  return {
    id,
    kind: 'video',
    duration: Math.round(seconds * TICKS_PER_SECOND),
    missing: false,
  } as unknown as MediaRef;
}

function projectWith(media: MediaRef[]): Project {
  return { media, clips: [], tracks: [], markers: [], groups: [] } as unknown as Project;
}

/** Lets pending promise callbacks run. */
async function flush(): Promise<void> {
  for (let i = 0; i < 5; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

describe('requested resolution', () => {
  it('scales peak buckets with duration within 2000..8192', () => {
    expect(peakBucketsFor(0)).toBe(2000);
    expect(peakBucketsFor(Math.round(10 * TICKS_PER_SECOND))).toBe(2000);
    expect(peakBucketsFor(Math.round(300 * TICKS_PER_SECOND))).toBe(6000);
    expect(peakBucketsFor(Math.round(7200 * TICKS_PER_SECOND))).toBe(8192);
  });

  it('scales thumbnail count with duration within 4..120', () => {
    expect(thumbCountFor(1000)).toBe(4);
    expect(thumbCountFor(60_000)).toBe(20);
    expect(thumbCountFor(7_200_000)).toBe(120);
  });
});

describe('media cache eviction', () => {
  beforeEach(() => {
    clearMediaCache();
    vi.mocked(editorApi.thumbnails).mockReset();
    vi.mocked(editorApi.thumbnails).mockImplementation(async (_id: string, times: number[]) => times.map(() => ''));
    vi.mocked(editorApi.waveform).mockReset();
  });

  it('keeps every entry of a project with more than 64 media, so nothing is refetched', async () => {
    const media = Array.from({ length: 70 }, (_, i) => videoMedia(`m${i}`, 10));
    const project = projectWith(media);
    for (const m of media) {
      requestThumbs(project, m.id, () => {});
    }
    await flush();
    expect(editorApi.thumbnails).toHaveBeenCalledTimes(70);

    // Requesting again must hit the cache for every item.
    for (const m of media) {
      requestThumbs(project, m.id, () => {});
    }
    await flush();
    expect(editorApi.thumbnails).toHaveBeenCalledTimes(70);
    expect(getThumbStrip('m0')).not.toBeNull();
  });

  it('drops least recently used entries of other projects beyond the 64 cap', async () => {
    const big = Array.from({ length: 70 }, (_, i) => videoMedia(`m${i}`, 10));
    const bigProject = projectWith(big);
    for (const m of big) {
      requestThumbs(bigProject, m.id, () => {});
    }
    await flush();

    // A small project adds one entry; the 70 entries of the big project exceed the cap of 64 for
    // media it does not use, so the oldest of those go.
    const small = projectWith([videoMedia('x', 10)]);
    requestThumbs(small, 'x', () => {});
    await flush();

    expect(getThumbStrip('m0')).toBeNull();
    expect(getThumbStrip('m69')).not.toBeNull();
    expect(getThumbStrip('x')).not.toBeNull();
  });
});
