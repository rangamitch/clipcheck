/**
 * Markup tests for CacheEntry.svelte, rendered with Svelte's server renderer (no DOM needed).
 *
 * The server renderer does not run onMount or $effect, so the cache is never read here: every
 * test sees the initial "checking" state. Loading results, the two-step Clear cache confirmation,
 * the 4 second revert, the second-click clear and the rejected-clear alert all need a real DOM
 * and timers. The repo has no DOM test environment (no jsdom or happy-dom) and no Svelte test
 * library, so those behaviours are NOT covered by this file.
 */
import { describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

import CacheEntry from './CacheEntry.svelte';

function html(): string {
  return render(CacheEntry).body;
}

/** The opening tag of the Clear cache button, or undefined if it is not rendered. */
function buttonTag(out: string): string | undefined {
  return (out.match(/<button[^>]*>/) ?? [])[0];
}

describe('CacheEntry markup (initial, before any cache read)', () => {
  it('shows the checking text while the size is loading', () => {
    expect(html()).toContain('Cache: checking…');
  });

  it('does not show a size or an unavailable message before the read completes', () => {
    const out = html();
    expect(out).not.toContain('size unavailable');
    expect(out).not.toMatch(/Cache: \d/);
  });

  it('labels the button "Clear cache" when idle', () => {
    expect(html()).toContain('Clear cache');
    expect(html()).not.toContain('Click again to delete');
  });

  it('renders the button as type="button"', () => {
    const tag = buttonTag(html());
    expect(tag).toBeDefined();
    expect(tag).toContain('type="button"');
  });

  it('disables the button while loading (clear needs a known, non-zero size)', () => {
    const tag = buttonTag(html());
    expect(tag).toContain('disabled');
  });

  it('gives the status line aria-live="polite" so the size change is announced', () => {
    expect(html()).toContain('aria-live="polite"');
  });

  it('carries the explanatory tip on the button and in a screen-reader-only span', () => {
    const out = html();
    const tip =
      'Deletes regenerable cache (audio extracts, thumbnails, waveforms) and old recording-buffer folders left by crashed sessions. Recordings and the live buffer are never touched.';
    expect(out).toContain(`title="${tip}"`);
    expect(out).toContain('id="cache-entry-tip"');
    // Svelte appends a scope hash to class names, so match the prefix only.
    expect(out).toContain('<span id="cache-entry-tip" class="sr-only');
    expect(out).toContain(`>${tip}</span>`);
    expect(out).toContain('aria-describedby="cache-entry-tip"');
  });

  it('shows no error alert on first render', () => {
    expect(html()).not.toContain('role="alert"');
    expect(html()).not.toContain('Could not clear the cache');
  });

  it('does not invoke any cache command during server render', () => {
    html();
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
