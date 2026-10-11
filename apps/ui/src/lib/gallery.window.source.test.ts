/**
 * Issue #80 (gallery windowing). Source-level checks on Gallery.svelte, plus a behavioural check
 * of the pure helpers in gallerywindow.ts. No DOM or component rendering.
 */
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { chunkPages, shouldRenderPage } from './gallerywindow';

const galterySource = readFileSync(
  fileURLToPath(new URL('./Gallery.svelte', import.meta.url)),
  'utf8',
);

describe('issue_80 gallery windowing (source)', () => {
  it('issue_80 Gallery.svelte imports chunkPages and shouldRenderPage from ./gallerywindow', () => {
    const importBlock = /import\s*\{([^}]*)\}\s*from\s*'\.\/gallerywindow'/.exec(galterySource);
    expect(importBlock).not.toBeNull();
    const names = importBlock![1];
    expect(names).toMatch(/\bchunkPages\b/);
    expect(names).toMatch(/\bshouldRenderPage\b/);
  });

  it('issue_80 Gallery.svelte guards IntersectionObserver with typeof and passes rootMargin', () => {
    expect(galterySource).toMatch(/typeof IntersectionObserver/);
    expect(galterySource).toMatch(/new IntersectionObserver\(/);
    expect(galterySource).toMatch(/rootMargin:\s*'[^']+'/);
  });

  it('issue_80 Gallery.svelte renders a spacer with an inline height and a per-page section', () => {
    expect(galterySource).toMatch(/class="spacer"[^>]*style="height:\s*\{[^}]+\}px"/);
    expect(galterySource).toMatch(/<section class="page"/);
  });

  it('issue_80 Gallery.svelte loops over pages, with cards keyed by r.id inside each page section', () => {
    const pagesLoop = galterySource.indexOf('{#each pages');
    expect(pagesLoop).toBeGreaterThanOrEqual(0);
    const sectionOpen = galterySource.indexOf('<section class="page"', pagesLoop);
    expect(sectionOpen).toBeGreaterThan(pagesLoop);
    const cardLoop = galterySource.indexOf('{#each page as r (r.id)}', sectionOpen);
    expect(cardLoop).toBeGreaterThan(sectionOpen);
  });

  it('issue_80 Gallery.svelte no longer has a flat {#each items as r (r.id)} card loop', () => {
    expect(galterySource).not.toMatch(/\{#each items as r \(r\.id\)\}/);
  });

  it('issue_80 Gallery.svelte disconnects its observers on cleanup', () => {
    expect(galterySource).toMatch(/pageObserver\?\.disconnect\(\)/);
    expect(galterySource).toMatch(/sizeObserver\?\.disconnect\(\)/);
  });

  it('issue_80 Gallery.svelte append path pushes onto items and does not copy the array', () => {
    expect(galterySource).toMatch(/items\.push\(/);
    expect(galterySource).not.toMatch(/\[\.\.\.items,\s*\.\.\.page\.items\]/);
  });
});

describe('issue_80 gallery windowing (behaviour)', () => {
  it('issue_80 with 500 items in pages of 60 and only page 3 visible, exactly 1 page renders', () => {
    const items = Array.from({ length: 500 }, (_, n) => n);
    const pages = chunkPages(items, 60);
    expect(pages).toHaveLength(9);

    const visible = new Set<number>([2]); // page 3, zero-based index 2
    const heights = new Map<number, number>();
    for (let i = 0; i < pages.length; i++) heights.set(i, 800);

    const rendered = pages
      .map((_, i) => i)
      .filter((i) => shouldRenderPage(i, visible, heights));
    expect(rendered.length).toBe(1);
    expect(rendered).toEqual([2]);
  });
});
