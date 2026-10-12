import { describe, expect, it } from 'vitest';
import { ACTIONS, SHORTCUT_HELP, resolveShortcut } from './shortcuts';
import type { ActionId, ShortcutEvent } from './shortcuts';

interface Mods {
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
  meta?: boolean;
}

/** Builds a keydown event with the given modifiers and no text-entry target. */
function ev(key: string, mods: Mods = {}, target: ShortcutEvent['target'] = null): ShortcutEvent {
  return {
    key,
    ctrlKey: mods.ctrl ?? false,
    shiftKey: mods.shift ?? false,
    altKey: mods.alt ?? false,
    metaKey: mods.meta ?? false,
    target,
  };
}

const DIV = { tagName: 'DIV', isContentEditable: false };

describe('design doc section 6 key map', () => {
  it.each<[string, Mods, ActionId]>([
    // Playback
    [' ', {}, ACTIONS.play_pause],
    ['k', {}, ACTIONS.play_pause],
    ['j', {}, ACTIONS.shuttle_back],
    ['l', {}, ACTIONS.shuttle_forward],
    ['ArrowLeft', {}, ACTIONS.frame_back],
    ['ArrowRight', {}, ACTIONS.frame_forward],
    ['ArrowLeft', { shift: true }, ACTIONS.second_back],
    ['ArrowRight', { shift: true }, ACTIONS.second_forward],
    ['ArrowUp', {}, ACTIONS.prev_edit],
    ['ArrowDown', {}, ACTIONS.next_edit],
    ['Home', {}, ACTIONS.go_start],
    ['End', {}, ACTIONS.go_end],
    // Editing
    ['z', { ctrl: true }, ACTIONS.undo],
    ['y', { ctrl: true }, ACTIONS.redo],
    ['z', { ctrl: true, shift: true }, ACTIONS.redo],
    ['s', { ctrl: true }, ACTIONS.save],
    ['e', { ctrl: true }, ACTIONS.export],
    ['S', { ctrl: true, shift: true }, ACTIONS.export_still],
    ['k', { ctrl: true }, ACTIONS.split],
    ['s', {}, ACTIONS.split],
    ['c', {}, ACTIONS.tool_razor],
    ['v', {}, ACTIONS.tool_select],
    ['h', {}, ACTIONS.tool_hand],
    ['z', {}, ACTIONS.tool_zoom],
    ['x', { alt: true }, ACTIONS.clear_in_out],
    ['i', {}, ACTIONS.set_in],
    ['o', {}, ACTIONS.set_out],
    ['m', {}, ACTIONS.add_marker],
    ['n', {}, ACTIONS.toggle_snap],
    ['Delete', {}, ACTIONS.delete],
    ['Delete', { shift: true }, ACTIONS.ripple_delete],
    ['g', { ctrl: true }, ACTIONS.group],
    ['G', { ctrl: true, shift: true }, ACTIONS.ungroup],
    ['L', { ctrl: true, shift: true }, ACTIONS.link_toggle],
    ['c', { ctrl: true }, ACTIONS.copy],
    ['x', { ctrl: true }, ACTIONS.cut],
    ['v', { ctrl: true }, ACTIONS.paste],
    ['d', { ctrl: true }, ACTIONS.duplicate],
    ['a', { ctrl: true }, ACTIONS.select_all],
    // Timeline zoom and source edits
    ['+', {}, ACTIONS.zoom_in],
    ['=', {}, ACTIONS.zoom_in],
    ['-', {}, ACTIONS.zoom_out],
    ['\\', {}, ACTIONS.zoom_fit],
    [',', {}, ACTIONS.insert_from_source],
    ['.', {}, ACTIONS.overwrite_from_source],
    ['Escape', {}, ACTIONS.escape],
    ['?', { shift: true }, ACTIONS.show_shortcuts],
    ['Backspace', {}, ACTIONS.delete],
  ])('resolves key %j with modifiers %j', (key, mods, action) => {
    expect(resolveShortcut(ev(key, mods))).toBe(action);
  });
});

describe('modifier handling', () => {
  it('separates plain K (play) from Ctrl+K (split)', () => {
    expect(resolveShortcut(ev('k'))).toBe(ACTIONS.play_pause);
    expect(resolveShortcut(ev('k', { ctrl: true }))).toBe(ACTIONS.split);
  });
  it('separates Ctrl+S (save) from plain S (split)', () => {
    expect(resolveShortcut(ev('s', { ctrl: true }))).toBe(ACTIONS.save);
    expect(resolveShortcut(ev('s'))).toBe(ACTIONS.split);
  });
  it('separates Ctrl+G (group) from Ctrl+Shift+G (ungroup)', () => {
    expect(resolveShortcut(ev('g', { ctrl: true }))).toBe(ACTIONS.group);
    expect(resolveShortcut(ev('g', { ctrl: true, shift: true }))).toBe(ACTIONS.ungroup);
  });
  it('separates Ctrl+S (save) from Ctrl+Shift+S (export still)', () => {
    expect(resolveShortcut(ev('s', { ctrl: true, shift: true }))).toBe(ACTIONS.export_still);
  });
  it('separates Delete (delete) from Shift+Delete (ripple delete)', () => {
    expect(resolveShortcut(ev('Delete'))).toBe(ACTIONS.delete);
    expect(resolveShortcut(ev('Delete', { shift: true }))).toBe(ACTIONS.ripple_delete);
  });
  it('treats Backspace with Shift as ripple delete', () => {
    expect(resolveShortcut(ev('Backspace', { shift: true }))).toBe(ACTIONS.ripple_delete);
  });
  it('treats Meta as Ctrl', () => {
    expect(resolveShortcut(ev('s', { meta: true }))).toBe(ACTIONS.save);
    expect(resolveShortcut(ev('z', { meta: true }))).toBe(ACTIONS.undo);
  });
  it('accepts an uppercase letter from Caps Lock as the plain binding', () => {
    expect(resolveShortcut(ev('K'))).toBe(ACTIONS.play_pause);
  });
  it('ignores a plain letter typed with Shift', () => {
    expect(resolveShortcut(ev('K', { shift: true }))).toBeNull();
    expect(resolveShortcut(ev('l', { shift: true }))).toBeNull();
  });
  it('ignores Ctrl+Alt combinations', () => {
    expect(resolveShortcut(ev('z', { ctrl: true, alt: true }))).toBeNull();
    expect(resolveShortcut(ev('k', { ctrl: true, alt: true }))).toBeNull();
  });
  it('ignores Alt with any key except Alt+X', () => {
    expect(resolveShortcut(ev('z', { alt: true }))).toBeNull();
    expect(resolveShortcut(ev('ArrowLeft', { alt: true }))).toBeNull();
  });
  it('ignores Alt+Shift+X', () => {
    expect(resolveShortcut(ev('x', { alt: true, shift: true }))).toBeNull();
  });
  it('ignores Ctrl with non-letter keys', () => {
    expect(resolveShortcut(ev('=', { ctrl: true }))).toBeNull();
    expect(resolveShortcut(ev('ArrowLeft', { ctrl: true }))).toBeNull();
    expect(resolveShortcut(ev('Delete', { ctrl: true }))).toBeNull();
    expect(resolveShortcut(ev('1', { ctrl: true, shift: true }))).toBeNull();
  });
  it('ignores Shift with unbound letters', () => {
    expect(resolveShortcut(ev('X', { shift: true }))).toBeNull();
  });
  it('ignores Shift+Ctrl with a letter that has no Ctrl+Shift binding', () => {
    expect(resolveShortcut(ev('x', { ctrl: true, shift: true }))).toBeNull();
    expect(resolveShortcut(ev('a', { ctrl: true, shift: true }))).toBeNull();
    expect(resolveShortcut(ev('d', { ctrl: true, shift: true }))).toBeNull();
  });
  it('ignores Shift for punctuation and named keys that have their own shifted form', () => {
    expect(resolveShortcut(ev('+', { shift: true }))).toBe(ACTIONS.zoom_in);
    expect(resolveShortcut(ev('Home', { shift: true }))).toBe(ACTIONS.go_start);
    expect(resolveShortcut(ev(' ', { shift: true }))).toBe(ACTIONS.play_pause);
    expect(resolveShortcut(ev('Escape', { shift: true }))).toBe(ACTIONS.escape);
    expect(resolveShortcut(ev('ArrowUp', { shift: true }))).toBe(ACTIONS.prev_edit);
  });
});

describe('Ctrl+Y and Ctrl+Shift+Z aliases', () => {
  it('maps both redo forms to redo', () => {
    expect(resolveShortcut(ev('y', { ctrl: true }))).toBe(ACTIONS.redo);
    expect(resolveShortcut(ev('Z', { ctrl: true, shift: true }))).toBe(ACTIONS.redo);
  });
  it('maps Ctrl+Z to undo and not to redo', () => {
    expect(resolveShortcut(ev('z', { ctrl: true }))).toBe(ACTIONS.undo);
  });
});

describe('text entry targets are ignored', () => {
  it.each(['INPUT', 'TEXTAREA', 'SELECT', 'input', 'textarea'])('ignores keys when the target is %s', (tag) => {
    expect(resolveShortcut(ev('k', {}, { tagName: tag }))).toBeNull();
    expect(resolveShortcut(ev(' ', {}, { tagName: tag }))).toBeNull();
  });
  it('ignores Ctrl+Z inside a text field', () => {
    expect(resolveShortcut(ev('z', { ctrl: true }, { tagName: 'INPUT' }))).toBeNull();
  });
  it('ignores Backspace and Delete inside a text field', () => {
    expect(resolveShortcut(ev('Backspace', {}, { tagName: 'INPUT' }))).toBeNull();
    expect(resolveShortcut(ev('Delete', {}, { tagName: 'TEXTAREA' }))).toBeNull();
  });
  it('ignores keys in a contentEditable element', () => {
    expect(resolveShortcut(ev('k', {}, { tagName: 'DIV', isContentEditable: true }))).toBeNull();
    expect(resolveShortcut(ev('ArrowLeft', {}, { isContentEditable: true }))).toBeNull();
  });
  it('handles keys on ordinary elements', () => {
    expect(resolveShortcut(ev('k', {}, DIV))).toBe(ACTIONS.play_pause);
    expect(resolveShortcut(ev('k', {}, { tagName: 'BUTTON' }))).toBe(ACTIONS.play_pause);
  });
  it('handles keys when the target is missing or null', () => {
    expect(resolveShortcut(ev('k', {}, null))).toBe(ACTIONS.play_pause);
    expect(resolveShortcut({ key: 'k', ctrlKey: false, shiftKey: false, altKey: false, metaKey: false })).toBe(
      ACTIONS.play_pause,
    );
  });
});

describe('unknown keys', () => {
  it.each([
    ['q', {}],
    ['F5', {}],
    ['Enter', {}],
    ['Tab', {}],
    ['Shift', {}],
    ['Control', {}],
    ['PageDown', {}],
    ['a', {}],
    ['x', {}],
    ['g', {}],
    ['d', {}],
    ['e', {}],
    ['u', {}],
    ['Z', { shift: true }],
    ['l', { ctrl: true }],
    ['f', { ctrl: true }],
    ['n', { ctrl: true }],
    ['q', { ctrl: true }],
    ['x', { ctrl: true, shift: true }],
    ['y', { ctrl: true, shift: true }],
  ])('returns null for key %j with modifiers %j', (key, mods) => {
    expect(resolveShortcut(ev(key, mods))).toBeNull();
  });
  it('returns null for an empty key name', () => {
    expect(resolveShortcut(ev(''))).toBeNull();
  });
});

describe('SHORTCUT_HELP table', () => {
  const actionValues = new Set<string>(Object.values(ACTIONS));
  const groups = new Set(['Playback', 'Editing', 'Tools', 'Timeline', 'Project']);

  it('has one row per action with no duplicates', () => {
    const ids = SHORTCUT_HELP.map((h) => h.action);
    expect(new Set(ids).size).toBe(ids.length);
  });
  it('references only real action ids', () => {
    for (const row of SHORTCUT_HELP) {
      expect(actionValues.has(row.action)).toBe(true);
    }
  });
  it('uses a known group label on every row', () => {
    for (const row of SHORTCUT_HELP) {
      expect(groups.has(row.group)).toBe(true);
    }
  });
  it('has non-empty key and label text on every row', () => {
    for (const row of SHORTCUT_HELP) {
      expect(row.keys.trim().length).toBeGreaterThan(0);
      expect(row.label.trim().length).toBeGreaterThan(0);
    }
  });
  it('lists every group at least once', () => {
    const used = new Set(SHORTCUT_HELP.map((h) => h.group));
    expect(used).toEqual(groups);
  });
  it('documents play/pause, undo and split with the section 6 keys', () => {
    const find = (a: ActionId): string | undefined => SHORTCUT_HELP.find((h) => h.action === a)?.keys;
    expect(find(ACTIONS.play_pause)).toBe('Space / K');
    expect(find(ACTIONS.undo)).toBe('Ctrl+Z');
    expect(find(ACTIONS.split)).toBe('Ctrl+K / S');
  });
  it('documents ungroup as Ctrl+Shift+G and ripple delete as Shift+Delete', () => {
    const find = (a: ActionId): string | undefined => SHORTCUT_HELP.find((h) => h.action === a)?.keys;
    expect(find(ACTIONS.ungroup)).toBe('Ctrl+Shift+G');
    expect(find(ACTIONS.ripple_delete)).toBe('Shift+Delete');
  });
});

describe('input types decide whether a shortcut is blocked', () => {
  it.each(['text', 'search', 'number', 'email', 'url', 'password', 'tel', 'TEXT', 'Search'])(
    'blocks shortcuts in a text-like input (type %s)',
    (type) => {
      expect(resolveShortcut(ev('k', {}, { tagName: 'INPUT', type }))).toBeNull();
    },
  );

  it.each(['checkbox', 'button', 'color', 'radio', 'submit'])(
    'does not block shortcuts in a non-text input (type %s)',
    (type) => {
      expect(resolveShortcut(ev('k', {}, { tagName: 'INPUT', type }))).toBe(ACTIONS.play_pause);
    },
  );

  it('treats an input with an empty type attribute as text', () => {
    expect(resolveShortcut(ev('k', {}, { tagName: 'INPUT', type: '' }))).toBeNull();
  });

  it('treats an input with a null type as text', () => {
    expect(resolveShortcut(ev('k', {}, { tagName: 'INPUT', type: null }))).toBeNull();
  });

  it('keeps blocking Delete inside a text input but not inside a checkbox', () => {
    expect(resolveShortcut(ev('Delete', {}, { tagName: 'INPUT', type: 'text' }))).toBeNull();
    expect(resolveShortcut(ev('Delete', {}, { tagName: 'INPUT', type: 'checkbox' }))).toBe(ACTIONS.delete);
  });

  it('blocks Space, arrows and Delete while a range slider has focus', () => {
    expect(resolveShortcut(ev(' ', {}, { tagName: 'INPUT', type: 'range' }))).toBeNull();
    expect(resolveShortcut(ev('ArrowLeft', {}, { tagName: 'INPUT', type: 'range' }))).toBeNull();
    expect(resolveShortcut(ev('Delete', {}, { tagName: 'INPUT', type: 'range' }))).toBeNull();
  });

  it('still blocks shortcuts in a SELECT and a TEXTAREA regardless of type', () => {
    expect(resolveShortcut(ev('k', {}, { tagName: 'SELECT', type: 'select-one' }))).toBeNull();
    expect(resolveShortcut(ev('k', {}, { tagName: 'TEXTAREA', type: 'textarea' }))).toBeNull();
  });
});

describe('zoom out with the Unicode minus sign', () => {
  it('maps U+2212 (minus sign) to zoom out', () => {
    expect(resolveShortcut(ev('−'))).toBe(ACTIONS.zoom_out);
  });

  it('still maps the ASCII hyphen to zoom out', () => {
    expect(resolveShortcut(ev('-'))).toBe(ACTIONS.zoom_out);
  });

  it('does not map U+2212 with Ctrl (no modifier binding)', () => {
    expect(resolveShortcut(ev('−', { ctrl: true }))).toBeNull();
  });

  it('U+2212 is ignored inside a text field', () => {
    expect(resolveShortcut(ev('−', {}, { tagName: 'INPUT', type: 'text' }))).toBeNull();
  });

  it('the help table lists zoom out with the hyphen key', () => {
    const row = SHORTCUT_HELP.find((r) => r.action === ACTIONS.zoom_out);
    expect(row).toBeDefined();
    expect(row?.keys).toBe('-');
  });
});
