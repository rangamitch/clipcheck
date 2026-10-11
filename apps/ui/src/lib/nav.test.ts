import { beforeEach, describe, expect, it } from 'vitest';
import { get } from 'svelte/store';
import { VIEWS, activeView, setView } from './nav';

beforeEach(() => {
  setView('gallery');
});

describe('nav store', () => {
  it('defaults to gallery', () => {
    expect(get(activeView)).toBe('gallery');
  });

  it('setView("settings") updates the store', () => {
    setView('settings');
    expect(get(activeView)).toBe('settings');
  });

  it('setView("gallery") switches back', () => {
    setView('settings');
    setView('gallery');
    expect(get(activeView)).toBe('gallery');
  });

  it('subscribers receive updates in order', () => {
    const seen: string[] = [];
    const unsubscribe = activeView.subscribe((v) => seen.push(v));
    setView('settings');
    setView('gallery');
    setView('settings');
    unsubscribe();
    expect(seen).toEqual(['gallery', 'settings', 'gallery', 'settings']);
  });

  it('VIEWS contains exactly gallery and settings in order', () => {
    expect([...VIEWS]).toEqual(['gallery', 'settings']);
  });
});
