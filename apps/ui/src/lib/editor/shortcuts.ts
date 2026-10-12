// Pure keyboard mapping for the editor. Editor focus only: the caller decides when to call
// `resolveShortcut`. No DOM, no stores, no Tauri.

/** Action ids. Keys and values are identical. */
export const ACTIONS = {
  play_pause: 'play_pause',
  shuttle_back: 'shuttle_back',
  shuttle_stop: 'shuttle_stop',
  shuttle_forward: 'shuttle_forward',
  frame_back: 'frame_back',
  frame_forward: 'frame_forward',
  second_back: 'second_back',
  second_forward: 'second_forward',
  prev_edit: 'prev_edit',
  next_edit: 'next_edit',
  go_start: 'go_start',
  go_end: 'go_end',
  tool_select: 'tool_select',
  tool_razor: 'tool_razor',
  tool_hand: 'tool_hand',
  tool_zoom: 'tool_zoom',
  set_in: 'set_in',
  set_out: 'set_out',
  clear_in_out: 'clear_in_out',
  add_marker: 'add_marker',
  toggle_snap: 'toggle_snap',
  delete: 'delete',
  ripple_delete: 'ripple_delete',
  split: 'split',
  group: 'group',
  ungroup: 'ungroup',
  link_toggle: 'link_toggle',
  copy: 'copy',
  cut: 'cut',
  paste: 'paste',
  duplicate: 'duplicate',
  select_all: 'select_all',
  zoom_in: 'zoom_in',
  zoom_out: 'zoom_out',
  zoom_fit: 'zoom_fit',
  undo: 'undo',
  redo: 'redo',
  save: 'save',
  export: 'export',
  export_still: 'export_still',
  insert_from_source: 'insert_from_source',
  overwrite_from_source: 'overwrite_from_source',
  escape: 'escape',
  show_shortcuts: 'show_shortcuts',
} as const;

/** Union of all action ids. */
export type ActionId = (typeof ACTIONS)[keyof typeof ACTIONS];

/** The keyboard event fields `resolveShortcut` reads. */
export interface ShortcutEvent {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  target?: { tagName?: string; isContentEditable?: boolean; type?: string | null } | null;
}

/** Elements that take text input. Shortcuts are ignored while one of them has focus. */
const TEXT_ENTRY_TAGS = new Set(['TEXTAREA', 'SELECT']);

/**
 * `<input>` types that take typed text or keys (range sliders included, so arrows and Space do not
 * fire editor shortcuts while a slider has focus). Other input types (checkbox, button, color, ...)
 * do not block shortcuts.
 */
const TEXT_INPUT_TYPES = new Set(['text', 'search', 'number', 'email', 'url', 'password', 'tel', 'range']);

/** True when the event target is a text-entry element. */
function isTextEntry(target: ShortcutEvent['target']): boolean {
  if (!target) return false;
  if (target.isContentEditable) return true;
  const tag = target.tagName;
  if (typeof tag !== 'string') return false;
  const upper = tag.toUpperCase();
  if (TEXT_ENTRY_TAGS.has(upper)) return true;
  if (upper === 'INPUT') {
    // A missing type behaves as text.
    const type = target.type;
    if (type === undefined || type === null || type === '') return true;
    return TEXT_INPUT_TYPES.has(String(type).toLowerCase());
  }
  return false;
}

/** Ctrl+letter bindings. Shift changes the meaning of some letters, handled separately. */
const CTRL_BINDINGS: Partial<Record<string, ActionId>> = {
  z: ACTIONS.undo,
  y: ACTIONS.redo,
  s: ACTIONS.save,
  e: ACTIONS.export,
  k: ACTIONS.split,
  g: ACTIONS.group,
  c: ACTIONS.copy,
  x: ACTIONS.cut,
  v: ACTIONS.paste,
  d: ACTIONS.duplicate,
  a: ACTIONS.select_all,
};

/** Ctrl+Shift+letter bindings. */
const CTRL_SHIFT_BINDINGS: Partial<Record<string, ActionId>> = {
  g: ACTIONS.ungroup,
  l: ACTIONS.link_toggle,
  s: ACTIONS.export_still,
  z: ACTIONS.redo,
};

/** Plain letter bindings (no Ctrl, no Alt, no Shift). */
const LETTER_BINDINGS: Partial<Record<string, ActionId>> = {
  k: ACTIONS.play_pause,
  j: ACTIONS.shuttle_back,
  l: ACTIONS.shuttle_forward,
  v: ACTIONS.tool_select,
  c: ACTIONS.tool_razor,
  h: ACTIONS.tool_hand,
  z: ACTIONS.tool_zoom,
  i: ACTIONS.set_in,
  o: ACTIONS.set_out,
  m: ACTIONS.add_marker,
  n: ACTIONS.toggle_snap,
  s: ACTIONS.split,
};

/** Punctuation and named keys (no Ctrl, no Alt). */
const KEY_BINDINGS: Partial<Record<string, ActionId>> = {
  ' ': ACTIONS.play_pause,
  ArrowUp: ACTIONS.prev_edit,
  ArrowDown: ACTIONS.next_edit,
  Home: ACTIONS.go_start,
  End: ACTIONS.go_end,
  Delete: ACTIONS.delete,
  Backspace: ACTIONS.delete,
  '+': ACTIONS.zoom_in,
  '=': ACTIONS.zoom_in,
  '-': ACTIONS.zoom_out,
  '−': ACTIONS.zoom_out,
  '\\': ACTIONS.zoom_fit,
  ',': ACTIONS.insert_from_source,
  '.': ACTIONS.overwrite_from_source,
  Escape: ACTIONS.escape,
  '?': ACTIONS.show_shortcuts,
};

/**
 * Map a keydown event to an editor action, or null when the key does nothing here.
 * Ctrl and Meta both count as Ctrl. Letters are case-insensitive. Events whose target is a text
 * entry element (input, textarea, select, contentEditable) are ignored entirely.
 */
export function resolveShortcut(e: ShortcutEvent): ActionId | null {
  if (isTextEntry(e.target)) return null;

  const ctrl = e.ctrlKey || e.metaKey;
  const shift = e.shiftKey;
  const alt = e.altKey;
  const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;
  const isLetter = key.length === 1 && key >= 'a' && key <= 'z';

  if (ctrl) {
    if (alt) return null;
    if (shift) {
      if (!isLetter) return null;
      return CTRL_SHIFT_BINDINGS[key] ?? null;
    }
    return isLetter ? (CTRL_BINDINGS[key] ?? null) : null;
  }

  if (alt) {
    return key === 'x' && !shift ? ACTIONS.clear_in_out : null;
  }

  // Arrow keys and delete keys use Shift for the coarse variant.
  if (key === 'ArrowLeft') return shift ? ACTIONS.second_back : ACTIONS.frame_back;
  if (key === 'ArrowRight') return shift ? ACTIONS.second_forward : ACTIONS.frame_forward;
  if (key === 'Delete' || key === 'Backspace') return shift ? ACTIONS.ripple_delete : ACTIONS.delete;

  // Remaining letters take no Shift.
  if (isLetter) {
    if (shift) return null;
    return LETTER_BINDINGS[key] ?? null;
  }

  // Punctuation and named keys ignore Shift ('?' arrives as Shift+/, '+' as Shift+=).
  return KEY_BINDINGS[key] ?? null;
}

/** Group headings for the shortcuts overlay. */
export type ShortcutGroup = 'Playback' | 'Editing' | 'Tools' | 'Timeline' | 'Project';

/** One row of the shortcuts overlay. `keys` is display text. */
export interface ShortcutHelp {
  action: ActionId;
  keys: string;
  label: string;
  group: ShortcutGroup;
}

/** Shortcut reference for the overlay, grouped. Order within a group is display order. */
export const SHORTCUT_HELP: readonly ShortcutHelp[] = [
  { action: ACTIONS.play_pause, keys: 'Space / K', label: 'Play / pause', group: 'Playback' },
  { action: ACTIONS.shuttle_back, keys: 'J', label: 'Shuttle back', group: 'Playback' },
  { action: ACTIONS.shuttle_forward, keys: 'L', label: 'Shuttle forward', group: 'Playback' },
  { action: ACTIONS.frame_back, keys: 'Left', label: 'Frame back', group: 'Playback' },
  { action: ACTIONS.frame_forward, keys: 'Right', label: 'Frame forward', group: 'Playback' },
  { action: ACTIONS.second_back, keys: 'Shift+Left', label: 'Back 1 second', group: 'Playback' },
  { action: ACTIONS.second_forward, keys: 'Shift+Right', label: 'Forward 1 second', group: 'Playback' },
  { action: ACTIONS.prev_edit, keys: 'Up', label: 'Previous edit point', group: 'Playback' },
  { action: ACTIONS.next_edit, keys: 'Down', label: 'Next edit point', group: 'Playback' },
  { action: ACTIONS.go_start, keys: 'Home', label: 'Go to start', group: 'Playback' },
  { action: ACTIONS.go_end, keys: 'End', label: 'Go to end', group: 'Playback' },

  { action: ACTIONS.delete, keys: 'Delete / Backspace', label: 'Delete (leaves gap)', group: 'Editing' },
  { action: ACTIONS.ripple_delete, keys: 'Shift+Delete', label: 'Ripple delete', group: 'Editing' },
  { action: ACTIONS.split, keys: 'Ctrl+K / S', label: 'Split at playhead', group: 'Editing' },
  { action: ACTIONS.group, keys: 'Ctrl+G', label: 'Group', group: 'Editing' },
  { action: ACTIONS.ungroup, keys: 'Ctrl+Shift+G', label: 'Ungroup', group: 'Editing' },
  { action: ACTIONS.link_toggle, keys: 'Ctrl+Shift+L', label: 'Link / unlink A/V', group: 'Editing' },
  { action: ACTIONS.copy, keys: 'Ctrl+C', label: 'Copy', group: 'Editing' },
  { action: ACTIONS.cut, keys: 'Ctrl+X', label: 'Cut', group: 'Editing' },
  { action: ACTIONS.paste, keys: 'Ctrl+V', label: 'Paste at playhead', group: 'Editing' },
  { action: ACTIONS.duplicate, keys: 'Ctrl+D', label: 'Duplicate', group: 'Editing' },
  { action: ACTIONS.select_all, keys: 'Ctrl+A', label: 'Select all', group: 'Editing' },
  { action: ACTIONS.undo, keys: 'Ctrl+Z', label: 'Undo', group: 'Editing' },
  { action: ACTIONS.redo, keys: 'Ctrl+Y / Ctrl+Shift+Z', label: 'Redo', group: 'Editing' },

  { action: ACTIONS.tool_select, keys: 'V', label: 'Selection tool', group: 'Tools' },
  { action: ACTIONS.tool_razor, keys: 'C', label: 'Cut tool', group: 'Tools' },
  { action: ACTIONS.tool_hand, keys: 'H', label: 'Hand tool', group: 'Tools' },
  { action: ACTIONS.tool_zoom, keys: 'Z', label: 'Zoom tool', group: 'Tools' },

  { action: ACTIONS.set_in, keys: 'I', label: 'Set in point', group: 'Timeline' },
  { action: ACTIONS.set_out, keys: 'O', label: 'Set out point', group: 'Timeline' },
  { action: ACTIONS.clear_in_out, keys: 'Alt+X', label: 'Clear in / out', group: 'Timeline' },
  { action: ACTIONS.add_marker, keys: 'M', label: 'Add marker', group: 'Timeline' },
  { action: ACTIONS.toggle_snap, keys: 'N', label: 'Toggle snapping', group: 'Timeline' },
  { action: ACTIONS.insert_from_source, keys: ',', label: 'Insert from source', group: 'Timeline' },
  { action: ACTIONS.overwrite_from_source, keys: '.', label: 'Overwrite from source', group: 'Timeline' },
  { action: ACTIONS.zoom_in, keys: '+ / =', label: 'Zoom in', group: 'Timeline' },
  { action: ACTIONS.zoom_out, keys: '-', label: 'Zoom out', group: 'Timeline' },
  { action: ACTIONS.zoom_fit, keys: '\\', label: 'Zoom to fit', group: 'Timeline' },
  { action: ACTIONS.escape, keys: 'Esc', label: 'Leave group / cancel drag', group: 'Timeline' },

  { action: ACTIONS.save, keys: 'Ctrl+S', label: 'Save project', group: 'Project' },
  { action: ACTIONS.export, keys: 'Ctrl+E', label: 'Export', group: 'Project' },
  { action: ACTIONS.export_still, keys: 'Ctrl+Shift+S', label: 'Export still frame', group: 'Project' },
  { action: ACTIONS.show_shortcuts, keys: '?', label: 'Show keyboard shortcuts', group: 'Project' },
];
