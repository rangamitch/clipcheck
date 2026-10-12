import { describe, expect, it } from 'vitest';
import { SETTINGS_TABS } from './tabs';

describe('SETTINGS_TABS registry', () => {
  it('lists the nine tabs in display order', () => {
    expect(SETTINGS_TABS.map((t) => t.id)).toEqual([
      'general',
      'storage',
      'detection',
      'video',
      'editor',
      'audio',
      'replay',
      'hotkeys',
      'updates',
    ]);
  });

  it('has unique ids', () => {
    const ids = SETTINGS_TABS.map((t) => t.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it.each([
    ['general', 'General'],
    ['storage', 'Storage'],
    ['detection', 'Games'],
    ['video', 'Video'],
    ['editor', 'Video editor'],
    ['audio', 'Audio'],
    ['replay', 'Recordings'],
    ['hotkeys', 'Hotkeys'],
    ['updates', 'Updates'],
  ])('tab %s has label %s', (id, label) => {
    expect(SETTINGS_TABS.find((t) => t.id === id)?.label).toBe(label);
  });

  it.each(['general', 'storage', 'detection', 'video', 'editor', 'audio', 'replay', 'hotkeys', 'updates'])(
    'tab %s has a component (no Coming next placeholder)',
    (id) => {
      const tab = SETTINGS_TABS.find((t) => t.id === id);
      expect(tab).toBeDefined();
      expect(tab?.component).toBeTruthy();
    },
  );

  it('every tab has a non-empty label', () => {
    for (const t of SETTINGS_TABS) expect(t.label.trim()).not.toBe('');
  });
});
