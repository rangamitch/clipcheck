/**
 * Markup tests for LogsPanel.svelte, rendered with Svelte's server renderer (no DOM needed).
 * Click, Escape, focus and pending-state behaviour need a real DOM and are NOT covered here:
 * the repo has no DOM test environment (no jsdom or happy-dom) and no Svelte test library.
 */
import { describe, expect, it, vi } from 'vitest';
import { render } from 'svelte/server';

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}));

import LogsPanel from './LogsPanel.svelte';

function html(open: boolean): string {
  return render(LogsPanel, { props: { open } }).body;
}

describe('LogsPanel markup', () => {
  it('renders nothing when closed', () => {
    const out = html(false);
    expect(out).not.toContain('role="dialog"');
    expect(out).not.toContain('Open logs folder');
    expect(out).not.toContain('Logs &amp; diagnostics');
  });

  it('renders a modal dialog when open', () => {
    const out = html(true);
    expect(out).toContain('role="dialog"');
    expect(out).toContain('aria-modal="true"');
    expect(out).toContain('aria-labelledby="logs-panel-title"');
    expect(out).toContain('aria-describedby="logs-panel-text"');
  });

  it('shows the title and intro text', () => {
    const out = html(true);
    expect(out).toContain('Logs &amp; diagnostics');
    expect(out).toContain('Your Windows user name is hidden.');
  });

  it('has the three action buttons with their idle labels', () => {
    const out = html(true);
    expect(out).toContain('Open logs folder');
    expect(out).toContain('Copy diagnostics');
    expect(out).toContain('Save to file');
  });

  it('has a Close button', () => {
    expect(html(true)).toContain('>Close<');
  });

  it('shows no error, saved path or Copied message on first open', () => {
    const out = html(true);
    expect(out).not.toContain('role="alert"');
    expect(out).not.toContain('Copied');
    expect(out).not.toContain('Show in folder');
    expect(out).not.toContain('Saved to');
  });

  it('enables the action buttons when idle', () => {
    const out = html(true);
    const buttons = out.match(/<button[^>]*>/g) ?? [];
    // Svelte appends a scope hash to class names, so match the prefix only.
    const actionButtons = buttons.filter((b) => /class="action[ "]/.test(b));
    expect(actionButtons).toHaveLength(3);
    for (const b of actionButtons) {
      expect(b).not.toContain('disabled');
    }
  });
});
