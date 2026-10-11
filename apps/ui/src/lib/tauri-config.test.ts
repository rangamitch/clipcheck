import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { WINDOW_PERMISSIONS } from './window';

const confPath = fileURLToPath(new URL('../../src-tauri/tauri.conf.json', import.meta.url));
const capPath = fileURLToPath(new URL('../../src-tauri/capabilities/default.json', import.meta.url));

const conf = JSON.parse(readFileSync(confPath, 'utf8'));
const cap = JSON.parse(readFileSync(capPath, 'utf8'));

describe('tauri.conf.json', () => {
  const win = conf.app.windows[0];

  it('parses and has a main window as the first window', () => {
    expect(win.label).toBe('main');
  });

  it('has the custom-titlebar window settings', () => {
    expect(win.decorations).toBe(false);
    expect(win.shadow).toBe(true);
    expect(win.resizable).toBe(true);
    expect(win.transparent).toBe(false);
  });

  it('has the expected title', () => {
    expect(win.title).toBe('ClipCheck');
  });

  it('has minimum size of at least 900x600', () => {
    expect(typeof win.minWidth).toBe('number');
    expect(typeof win.minHeight).toBe('number');
    expect(win.minWidth).toBeGreaterThanOrEqual(900);
    expect(win.minHeight).toBeGreaterThanOrEqual(600);
  });
});

describe('capabilities/default.json', () => {
  it('applies to the main window', () => {
    expect(cap.windows).toContain('main');
  });

  it('permissions is an array of strings', () => {
    expect(Array.isArray(cap.permissions)).toBe(true);
    for (const p of cap.permissions) expect(typeof p).toBe('string');
  });

  it('includes every WINDOW_PERMISSIONS value', () => {
    for (const value of Object.values(WINDOW_PERMISSIONS)) {
      expect(cap.permissions).toContain(value);
    }
  });

  it('includes start-dragging and internal-toggle-maximize for the drag region', () => {
    expect(cap.permissions).toContain('core:window:allow-start-dragging');
    expect(cap.permissions).toContain('core:window:allow-internal-toggle-maximize');
  });

  it('has no empty permission strings', () => {
    expect(cap.permissions.some((p: string) => p.trim() === '')).toBe(false);
  });

  it('has no duplicate permissions', () => {
    expect(new Set(cap.permissions).size).toBe(cap.permissions.length);
  });
});
