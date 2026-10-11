import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import {
  buildListQuery,
  listRecordings,
  mapPage,
  type GalleryFilters,
  type PageDto,
} from './api';

const base: GalleryFilters = {
  text: '',
  game: null,
  kind: null,
  favoritesOnly: false,
  sort: 'newest',
};

beforeEach(() => {
  invokeMock.mockReset();
});

describe('buildListQuery', () => {
  it('maps default filters to nulls and the default page size', () => {
    expect(buildListQuery(base)).toEqual({
      text: null,
      game: null,
      kind: null,
      favoritesOnly: false,
      sort: 'newest',
      limit: 60,
      cursor: null,
    });
  });
  it('trims search text and passes filters and cursor through', () => {
    const q = buildListQuery(
      { ...base, text: '  apex  ', game: 'g1', kind: 'clip', favoritesOnly: true, sort: 'longest' },
      'abc',
      20,
    );
    expect(q).toMatchObject({
      text: 'apex',
      game: 'g1',
      kind: 'clip',
      favoritesOnly: true,
      sort: 'longest',
      limit: 20,
      cursor: 'abc',
    });
  });
  it('treats whitespace-only text as no filter', () => {
    expect(buildListQuery({ ...base, text: '   ' }).text).toBeNull();
  });
});

/** A full wire item: the Rust DTO always serializes every field, nulls included. */
function fullItem(): PageDto['items'][number] {
  return {
    id: '01H0000000000000000000000A',
    kind: 'clip',
    sessionId: null,
    partIndex: null,
    gameId: null,
    gameName: null,
    fileName: 'a.mp4',
    ext: 'mp4',
    sizeBytes: 10,
    durationMs: 1000,
    createdAtMs: 1,
    status: 'ready',
    isFavorite: false,
    recovered: false,
    degraded: false,
    needsFaststart: false,
  };
}

describe('mapPage', () => {
  it('returns the same page object it was given', () => {
    const raw: PageDto = { items: [fullItem()], nextCursor: 'c2' };
    expect(mapPage(raw)).toBe(raw);
  });
  it('makes no per-item copy', () => {
    const raw: PageDto = { items: [fullItem(), fullItem()], nextCursor: null };
    const page = mapPage(raw);
    expect(page.items).toBe(raw.items);
    expect(page.items[0]).toBe(raw.items[0]);
    expect(page.items[1]).toBe(raw.items[1]);
  });
  it('keeps null optional fields as null', () => {
    const page = mapPage({ items: [fullItem()], nextCursor: null });
    expect(page.nextCursor).toBeNull();
    expect(page.items[0]).toMatchObject({
      sessionId: null,
      partIndex: null,
      gameId: null,
      gameName: null,
      fileName: 'a.mp4',
    });
  });
});

describe('listRecordings', () => {
  it('invokes list_recordings with the query wrapped as { query }', async () => {
    invokeMock.mockResolvedValue({ items: [], nextCursor: 'c1' });
    const q = buildListQuery(base);
    const page = await listRecordings(q);
    expect(invokeMock).toHaveBeenCalledWith('list_recordings', { query: q });
    expect(page).toEqual({ items: [], nextCursor: 'c1' });
  });
});
