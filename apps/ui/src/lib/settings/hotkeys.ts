/**
 * Key chord helpers for the Hotkeys and Replay tabs. Pure: no Svelte, no I/O.
 *
 * Chord strings are canonical: Ctrl, Alt, Shift, Win, then one key name, joined with `+`. Display
 * order is Ctrl, Alt, Shift, Win, then the key, which is what the daemon writes to config.toml. The
 * parser accepts any order, any case, and optional spaces around `+`.
 *
 * Key names are the spellings the daemon reads (crates/core/src/config/schema.rs `NAMED_KEYS`,
 * plus A-Z, 0-9 and F1-F24). Capture reads `KeyboardEvent.code`, so it does not depend on the
 * keyboard layout. Friendly labels such as "Scroll Lock" or "Num 5" are for display only.
 */
import type { ClipPress, SessionHotkeysConfig } from './store';

export interface ChordModifiers {
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  win: boolean;
}

/** The minimal KeyboardEvent fields capture reads. A real KeyboardEvent satisfies this. */
export interface ChordEvent {
  code: string;
  key: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  getModifierState?: (name: string) => boolean;
}

export type CaptureResult =
  | { ok: true; chord: string }
  /** Only modifiers are held. Show the live feedback and wait for the key. */
  | { ok: false; reason: 'modifier_only' }
  /** The key is not one a chord can end with. */
  | { ok: false; reason: 'unsupported_key' }
  /** A key that types text, pressed with no modifier. */
  | { ok: false; reason: 'needs_modifier'; chord: string };

const LETTERS = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ'.split('');
const DIGITS = '0123456789'.split('');
const FKEYS = Array.from({ length: 24 }, (_, i) => `F${i + 1}`);
const NUMPAD_DIGITS = Array.from({ length: 10 }, (_, i) => `Numpad${i}`);

/** Named keys, with the daemon's spelling. */
const NAMED_KEY_NAMES: readonly string[] = [
  'Pause',
  'ScrollLock',
  'NumLock',
  'CapsLock',
  'PrintScreen',
  'Insert',
  'Delete',
  'Home',
  'End',
  'PageUp',
  'PageDown',
  'Up',
  'Down',
  'Left',
  'Right',
  'Tab',
  'Enter',
  'Backspace',
  'Space',
  'AppsKey',
  ...NUMPAD_DIGITS,
  'NumpadAdd',
  'NumpadSubtract',
  'NumpadMultiply',
  'NumpadDivide',
  'NumpadDecimal',
  'NumpadSeparator',
  'MediaPlayPause',
  'MediaNext',
  'MediaPrev',
  'MediaStop',
  'VolumeUp',
  'VolumeDown',
  'VolumeMute',
  'LaunchMail',
  'LaunchMediaSelect',
  'LaunchApp1',
  'LaunchApp2',
  'BrowserBack',
  'BrowserForward',
  'BrowserRefresh',
  'BrowserStop',
  'BrowserSearch',
  'BrowserFavorites',
  'BrowserHome',
  'Semicolon',
  'Equals',
  'Comma',
  'Minus',
  'Period',
  'Slash',
  'Backquote',
  'LeftBracket',
  'Backslash',
  'RightBracket',
  'Quote',
];

/**
 * Every keyboard key name a chord may end with, in canonical spelling. Escape is not here: it
 * cancels capture and can never be bound. Mouse buttons are not in this list (see
 * MOUSE_KEY_NAMES), so the key-name contract table still matches this list exactly.
 */
export const SUPPORTED_KEYS: readonly string[] = [...LETTERS, ...DIGITS, ...FKEYS, ...NAMED_KEY_NAMES];

/**
 * Mouse buttons a chord may end with. Only the middle and side buttons: left and right are never
 * allowed. The daemon reads these as VK 0x04-0x06 (crates/core/src/config/schema.rs). The
 * settings UI captures them only on rows that pass `allowMouse` (the push-to-talk rows).
 */
export const MOUSE_KEY_NAMES: readonly string[] = ['Mouse3', 'Mouse4', 'Mouse5'];

/** Upper-cased name to canonical name, so the parser accepts any case. Keyboard and mouse names. */
const KEY_BY_UPPER = new Map<string, string>([...SUPPORTED_KEYS, ...MOUSE_KEY_NAMES].map((k) => [k.toUpperCase(), k]));

/** `MouseEvent.button` to mouse key name. Only the middle (1), back (3) and forward (4) buttons. */
const MOUSE_BUTTON_KEY: ReadonlyMap<number, string> = new Map<number, string>([
  [1, 'Mouse3'],
  [3, 'Mouse4'],
  [4, 'Mouse5'],
]);

/** Most push-to-talk keys one user may set. Mirrors CFG-H009 (validate.rs MAX_PTT_KEYS). */
export const MAX_PUSH_TO_TALK_KEYS = 4;

/** Alternative spellings the daemon also reads, mapped to the canonical name (upper-cased keys). */
const KEY_ALIASES: ReadonlyMap<string, string> = new Map<string, string>([['MENU', 'AppsKey']]);

/** `KeyboardEvent.code` to schema key name. Layout independent. */
const CODE_TO_KEY: ReadonlyMap<string, string> = new Map<string, string>([
  ['Pause', 'Pause'],
  ['ScrollLock', 'ScrollLock'],
  ['NumLock', 'NumLock'],
  ['CapsLock', 'CapsLock'],
  ['PrintScreen', 'PrintScreen'],
  ['Insert', 'Insert'],
  ['Delete', 'Delete'],
  ['Home', 'Home'],
  ['End', 'End'],
  ['PageUp', 'PageUp'],
  ['PageDown', 'PageDown'],
  ['ArrowUp', 'Up'],
  ['ArrowDown', 'Down'],
  ['ArrowLeft', 'Left'],
  ['ArrowRight', 'Right'],
  ['Tab', 'Tab'],
  ['Enter', 'Enter'],
  // Both Enter keys report VK_RETURN to Windows, so they share one name.
  ['NumpadEnter', 'Enter'],
  ['Backspace', 'Backspace'],
  ['Space', 'Space'],
  ['ContextMenu', 'AppsKey'],
  ['NumpadComma', 'NumpadSeparator'],
  ['NumpadAdd', 'NumpadAdd'],
  ['NumpadSubtract', 'NumpadSubtract'],
  ['NumpadMultiply', 'NumpadMultiply'],
  ['NumpadDivide', 'NumpadDivide'],
  ['NumpadDecimal', 'NumpadDecimal'],
  ['MediaPlayPause', 'MediaPlayPause'],
  ['MediaTrackNext', 'MediaNext'],
  ['MediaTrackPrevious', 'MediaPrev'],
  ['MediaStop', 'MediaStop'],
  ['AudioVolumeUp', 'VolumeUp'],
  ['AudioVolumeDown', 'VolumeDown'],
  ['AudioVolumeMute', 'VolumeMute'],
  ['LaunchMail', 'LaunchMail'],
  ['MediaSelect', 'LaunchMediaSelect'],
  ['LaunchApp1', 'LaunchApp1'],
  ['LaunchApp2', 'LaunchApp2'],
  ['BrowserBack', 'BrowserBack'],
  ['BrowserForward', 'BrowserForward'],
  ['BrowserRefresh', 'BrowserRefresh'],
  ['BrowserStop', 'BrowserStop'],
  ['BrowserSearch', 'BrowserSearch'],
  ['BrowserFavorites', 'BrowserFavorites'],
  ['BrowserHome', 'BrowserHome'],
  ['Semicolon', 'Semicolon'],
  ['Equal', 'Equals'],
  ['Comma', 'Comma'],
  ['Minus', 'Minus'],
  ['Period', 'Period'],
  ['Slash', 'Slash'],
  ['Backquote', 'Backquote'],
  ['BracketLeft', 'LeftBracket'],
  ['Backslash', 'Backslash'],
  ['BracketRight', 'RightBracket'],
  ['Quote', 'Quote'],
  ...NUMPAD_DIGITS.map((n): [string, string] => [n, n]),
]);

/** `KeyboardEvent.key` to schema key name. Used only when the browser gives no usable `code`. */
const VALUE_TO_KEY: ReadonlyMap<string, string> = new Map<string, string>([
  [' ', 'Space'],
  ['Pause', 'Pause'],
  ['ScrollLock', 'ScrollLock'],
  ['NumLock', 'NumLock'],
  ['CapsLock', 'CapsLock'],
  ['PrintScreen', 'PrintScreen'],
  ['Insert', 'Insert'],
  ['Delete', 'Delete'],
  ['Home', 'Home'],
  ['End', 'End'],
  ['PageUp', 'PageUp'],
  ['PageDown', 'PageDown'],
  ['ArrowUp', 'Up'],
  ['ArrowDown', 'Down'],
  ['ArrowLeft', 'Left'],
  ['ArrowRight', 'Right'],
  ['Tab', 'Tab'],
  ['Enter', 'Enter'],
  ['Backspace', 'Backspace'],
  ['ContextMenu', 'AppsKey'],
  ['MediaPlayPause', 'MediaPlayPause'],
  ['MediaTrackNext', 'MediaNext'],
  ['MediaTrackPrevious', 'MediaPrev'],
  ['MediaStop', 'MediaStop'],
  ['AudioVolumeUp', 'VolumeUp'],
  ['AudioVolumeDown', 'VolumeDown'],
  ['AudioVolumeMute', 'VolumeMute'],
]);

const MODIFIER_KEYS = new Set([
  'Control',
  'Shift',
  'Alt',
  'Meta',
  'OS',
  'AltGraph',
  'Super',
  'Hyper',
]);

/** Modifier keys by `code`, so a modifier press is seen even when `key` is not usable. */
const MODIFIER_CODE = /^(Control|Shift|Alt|Meta|OS)(Left|Right)?$|^AltGraph$/;

/**
 * True when a key may be bound with no modifier. Every supported key may be bound alone, as the
 * key-name contract (docs/design/hotkey-key-names.md) allows. A bare letter or Space will also
 * fire while the user types in other apps, so the Settings help warns about it. Mouse buttons
 * (Mouse3, Mouse4, Mouse5) may be bound alone too, like function and media keys.
 */
export function isBareAllowed(key: string): boolean {
  return KEY_BY_UPPER.has(key.toUpperCase());
}

/** True for a mouse button key name (Mouse3, Mouse4, Mouse5). */
export function isMouseKey(key: string): boolean {
  return MOUSE_KEY_NAMES.includes(key);
}

/**
 * Maps a `MouseEvent.button` to a mouse key name, or null for the buttons that are never bound:
 * left (0), right (2) and anything past the forward button.
 */
export function mouseButtonKey(button: number): string | null {
  return MOUSE_BUTTON_KEY.get(button) ?? null;
}

/**
 * True when one more push-to-talk key may be added to a list of `count` keys. The list holds at
 * most MAX_PUSH_TO_TALK_KEYS (4).
 */
export function canAddPushToTalkKey(count: number): boolean {
  return count < MAX_PUSH_TO_TALK_KEYS;
}

/**
 * Maps a KeyboardEvent.code to a schema key name, or null when no name fits. Letters and digits
 * come from the physical key (so Shift+1 gives "1"). Numpad keys keep their own names, so a numpad
 * 5 is "Numpad5" and not "5".
 */
export function keyFromCode(code: string): string | null {
  const letter = /^Key([A-Z])$/.exec(code);
  if (letter) return letter[1];
  const digit = /^Digit([0-9])$/.exec(code);
  if (digit) return digit[1];
  const fkey = /^F(\d{1,2})$/.exec(code);
  if (fkey) {
    const n = Number(fkey[1]);
    return n >= 1 && n <= 24 ? `F${n}` : null;
  }
  return CODE_TO_KEY.get(code) ?? null;
}

/**
 * Fallback when `code` is empty or unusable (some on-screen and remote keyboards): reads the
 * produced `key` for a letter, digit, F-key or named key. Returns null when it is none of those.
 */
export function keyFromKey(key: string): string | null {
  if (/^[a-zA-Z]$/.test(key)) return key.toUpperCase();
  if (/^[0-9]$/.test(key)) return key;
  const f = /^[Ff](\d{1,2})$/.exec(key);
  if (f) {
    const n = Number(f[1]);
    return n >= 1 && n <= 24 ? `F${n}` : null;
  }
  return VALUE_TO_KEY.get(key) ?? null;
}

/** The modifier fields of an event. Both a KeyboardEvent and a MouseEvent satisfy this. */
export type ModifierEvent = Pick<ChordEvent, 'ctrlKey' | 'altKey' | 'shiftKey' | 'metaKey' | 'getModifierState'>;

/** Modifiers held on an event. AltGr sends synthetic Ctrl+Alt, so those are dropped. */
export function modifiersOf(e: ModifierEvent): ChordModifiers {
  const altGraph = e.getModifierState?.('AltGraph') ?? false;
  return {
    ctrl: altGraph ? false : e.ctrlKey,
    alt: altGraph ? false : e.altKey,
    shift: e.shiftKey,
    win: e.metaKey,
  };
}

/** The held modifiers in canonical order (Ctrl, Alt, Shift, Win). Both formatters use this list. */
function modifierNames(mods: ChordModifiers): string[] {
  const parts: string[] = [];
  if (mods.ctrl) parts.push('Ctrl');
  if (mods.alt) parts.push('Alt');
  if (mods.shift) parts.push('Shift');
  if (mods.win) parts.push('Win');
  return parts;
}

/** Builds the canonical chord text from modifiers and a key name. */
export function formatChord(mods: ChordModifiers, key: string): string {
  return [...modifierNames(mods), key].join('+');
}

/** Live feedback while keys are held, such as "Ctrl + Shift". Empty when nothing is held. */
export function heldModifiersLabel(mods: ChordModifiers): string {
  return modifierNames(mods).join(' + ');
}

/** Friendly names for key names that read badly on screen. Other names show as they are. */
const KEY_LABELS: ReadonlyMap<string, string> = new Map<string, string>([
  ['ScrollLock', 'Scroll Lock'],
  ['PrintScreen', 'Print Screen'],
  ['NumLock', 'Num Lock'],
  ['CapsLock', 'Caps Lock'],
  ['PageUp', 'Page Up'],
  ['PageDown', 'Page Down'],
  ['AppsKey', 'Menu'],
  ['MediaPlayPause', 'Play/Pause'],
  ['MediaNext', 'Next Track'],
  ['MediaPrev', 'Previous Track'],
  ['MediaStop', 'Stop Media'],
  ['VolumeUp', 'Volume Up'],
  ['VolumeDown', 'Volume Down'],
  ['VolumeMute', 'Mute'],
  ['NumpadAdd', 'Num +'],
  ['NumpadSubtract', 'Num -'],
  ['NumpadMultiply', 'Num *'],
  ['NumpadDivide', 'Num /'],
  ['NumpadDecimal', 'Num .'],
  ...NUMPAD_DIGITS.map((n): [string, string] => [n, `Num ${n.slice('Numpad'.length)}`]),
  ['Semicolon', ';'],
  ['Equals', '='],
  ['Comma', ','],
  ['Minus', '-'],
  ['Period', '.'],
  ['Slash', '/'],
  ['Backquote', '`'],
  ['LeftBracket', '['],
  ['Backslash', '\\'],
  ['RightBracket', ']'],
  ['Quote', "'"],
  ['NumpadSeparator', 'Num ,'],
  ['LaunchMail', 'Mail'],
  ['LaunchMediaSelect', 'Media Select'],
  ['LaunchApp1', 'App 1'],
  ['LaunchApp2', 'App 2'],
  ['BrowserBack', 'Browser Back'],
  ['BrowserForward', 'Browser Forward'],
  ['BrowserRefresh', 'Browser Refresh'],
  ['BrowserStop', 'Browser Stop'],
  ['BrowserSearch', 'Browser Search'],
  ['BrowserFavorites', 'Browser Favorites'],
  ['BrowserHome', 'Browser Home'],
  ['Mouse3', 'Middle mouse button'],
  ['Mouse4', 'Mouse button 4 (back)'],
  ['Mouse5', 'Mouse button 5 (forward)'],
]);

/** Friendly text for one key name, such as "Scroll Lock" for ScrollLock. */
export function keyLabel(key: string): string {
  return KEY_LABELS.get(key) ?? key;
}

/**
 * Chord text for people to read, such as "Ctrl+Scroll Lock" or "Num 5". The stored value is
 * unchanged. Text that does not parse is returned as it is.
 */
export function chordLabel(chord: string): string {
  const parsed = parseChord(chord);
  if (!parsed) return chord;
  return [...modifierNames(parsed.mods), keyLabel(parsed.key)].join('+');
}

/**
 * The key caps for a chord chip, upper-cased: ["ALT", "F7"] for Alt+F7 and ["PAUSE"] for Pause.
 * Empty for an unbound chord.
 */
export function chordChipParts(chord: string): string[] {
  if (chord === '') return [];
  const parsed = parseChord(chord);
  if (!parsed) return [chord];
  return [...modifierNames(parsed.mods), keyLabel(parsed.key)].map((p) => p.toUpperCase());
}

/**
 * The session chords (split, start or stop, buffer, mute, then each push-to-talk key) that a new
 * clip must not reuse. Unbound (empty) entries are left out. Shared by the Replay and Hotkeys tabs.
 */
export function sessionChordsOf(session: SessionHotkeysConfig | undefined): string[] {
  if (!session) return [];
  const ptt = session.push_to_talk ?? [];
  return [session.split, session.stop_start, session.toggle_buffer, session.mute_mic, ...ptt].filter((c) => c !== '');
}

/**
 * Turns a keydown into a chord. Modifier-only presses report `modifier_only` so the UI can
 * show what is held. A key that types text is refused without a modifier (see `isBareAllowed`).
 */
export function eventToChord(e: ChordEvent): CaptureResult {
  if (MODIFIER_KEYS.has(e.key) || (e.code && MODIFIER_CODE.test(e.code))) {
    return { ok: false, reason: 'modifier_only' };
  }
  // `code` is the physical key and wins. `key` is used only when `code` names no supported key.
  const key = (e.code && keyFromCode(e.code)) || keyFromKey(e.key);
  const mods = modifiersOf(e);
  if (key === null) return { ok: false, reason: 'unsupported_key' };
  const hasMod = mods.ctrl || mods.alt || mods.shift || mods.win;
  if (!hasMod && !isBareAllowed(key)) {
    return { ok: false, reason: 'needs_modifier', chord: formatChord(mods, key) };
  }
  return { ok: true, chord: formatChord(mods, key) };
}

/** Parsed form of a chord string, or null when the text is not a valid chord. */
export interface ParsedChord {
  mods: ChordModifiers;
  key: string;
}

/**
 * Splits chord text into modifiers and a key name without checking the key. A known key is
 * returned in canonical spelling. An unknown key is returned upper-cased. A null result means a
 * modifier is unknown or repeated, or a part is empty.
 */
function splitChord(text: string): ParsedChord | null {
  const parts = text.split('+').map((p) => p.trim());
  if (parts.some((p) => p === '')) return null;
  const last = parts[parts.length - 1].toUpperCase();
  const key = KEY_BY_UPPER.get(last) ?? KEY_ALIASES.get(last) ?? last;
  const mods: ChordModifiers = { ctrl: false, alt: false, shift: false, win: false };
  for (const m of parts.slice(0, -1)) {
    const slot = m.toLowerCase();
    if (slot === 'ctrl' && !mods.ctrl) mods.ctrl = true;
    else if (slot === 'alt' && !mods.alt) mods.alt = true;
    else if (slot === 'shift' && !mods.shift) mods.shift = true;
    else if (slot === 'win' && !mods.win) mods.win = true;
    else return null;
  }
  return { mods, key };
}

/**
 * Parses chord text the way the schema does: `+`-separated, modifiers in any order and case,
 * exactly one key last, no duplicate modifiers. Returns null when it would not parse.
 */
export function parseChord(text: string): ParsedChord | null {
  const parsed = splitChord(text);
  if (!parsed || !KEY_BY_UPPER.has(parsed.key.toUpperCase())) return null;
  return parsed;
}

/** Canonical text for a chord in any accepted spelling, or null when it is not valid. */
export function canonicalizeChord(text: string): string | null {
  const parsed = parseChord(text);
  if (!parsed) return null;
  return formatChord(parsed.mods, parsed.key);
}

/**
 * Windows-reserved and unsafe chords, mirrored from crates/hotkeys/src/registry.rs
 * `reserved()`. Each row is [ctrl, shift, alt, win, key, name]; a chord matches only when its
 * modifiers are exactly those listed. Key names are upper-cased here.
 */
export const RESERVED_CHORDS: ReadonlyArray<readonly [boolean, boolean, boolean, boolean, string, string]> = [
  [true, false, true, false, 'DELETE', 'Ctrl+Alt+Delete (secure attention sequence)'],
  [false, false, false, true, 'L', 'Win+L (lock workstation)'],
  [false, false, true, false, 'F4', 'Alt+F4 (close window)'],
  [false, false, true, false, 'TAB', 'Alt+Tab (task switcher)'],
  [true, true, false, false, 'ESCAPE', 'Ctrl+Shift+Esc (Task Manager)'],
  [false, false, false, true, 'G', 'Win+G (Xbox Game Bar)'],
  [false, false, true, true, 'R', 'Win+Alt+R (Game DVR)'],
  [false, false, true, true, 'G', 'Win+Alt+G (Game DVR)'],
  [false, true, false, true, 'S', 'Win+Shift+S (screenshot)'],
  [false, false, false, true, 'PRINTSCREEN', 'Win+PrintScreen (screenshot)'],
  [false, false, true, false, 'SPACE', 'Alt+Space (window menu)'],
];

/**
 * The reason a chord is reserved, or null. Rows for Escape and Delete are kept so that a
 * hand-written config is flagged the same way the daemon flags it.
 */
export function isReservedChord(text: string): string | null {
  const parsed = splitChord(text);
  if (!parsed) return null;
  const { mods } = parsed;
  const key = parsed.key.toUpperCase();
  const hit = RESERVED_CHORDS.find(
    ([ctrl, shift, alt, win, k]) =>
      ctrl === mods.ctrl && shift === mods.shift && alt === mods.alt && win === mods.win && k === key,
  );
  return hit ? hit[5] : null;
}

/**
 * A problem that stops a chord from being bound, or null. `needs_modifier` is the text-key rule
 * (see `isBareAllowed`); mouse buttons Mouse3-5 are allowed alone, so they never hit it. `reserved`
 * is a chord Windows or ClipCheck owns. Left and right mouse buttons do not parse, so they are
 * `invalid`.
 */
export function chordProblem(text: string): 'needs_modifier' | 'reserved' | 'invalid' | null {
  const parsed = parseChord(text);
  if (!parsed) return 'invalid';
  const hasMod = parsed.mods.ctrl || parsed.mods.alt || parsed.mods.shift || parsed.mods.win;
  if (!hasMod && !isBareAllowed(parsed.key)) return 'needs_modifier';
  if (isReservedChord(text)) return 'reserved';
  return null;
}

export interface ChordEntry {
  label: string;
  chord: string;
  /** Set for clip hotkeys. Session hotkeys leave it out. */
  press?: ClipPress;
}

export interface DuplicateChord {
  chord: string;
  labels: string[];
}

/**
 * True when a group of entries that share one chord is an allowed Tap + Long press pair: exactly
 * two clip hotkeys whose press types differ. Any other shared chord is a conflict, including a
 * clip that shares a chord with a session hotkey.
 */
export function isTapLongPair(group: readonly ChordEntry[]): boolean {
  if (group.length !== 2) return false;
  const [a, b] = group;
  return a.press !== undefined && b.press !== undefined && a.press !== b.press;
}

/**
 * Groups entries that share a chord and returns only the conflicting groups. Blank and invalid
 * chords are ignored (their own fields show the problem). Chords compare in canonical form, so
 * "alt+f9" and "Alt+F9" match. A Tap + Long press pair is not a conflict (see isTapLongPair).
 */
export function findDuplicateChords(entries: ChordEntry[]): DuplicateChord[] {
  const groups = groupByChord(entries);
  const out: DuplicateChord[] = [];
  for (const [chord, group] of groups) {
    if (group.length > 1 && !isTapLongPair(group)) {
      out.push({ chord, labels: group.map((e) => e.label) });
    }
  }
  return out;
}

/**
 * For each entry, true when it is one half of a Tap + Long press pair. Used to show the linked
 * badge next to both rows.
 */
export function tapLongPairFlags(entries: ChordEntry[]): boolean[] {
  const flags = entries.map(() => false);
  const indexes = new Map<string, number[]>();
  entries.forEach((entry, i) => {
    const canon = canonicalizeChord(entry.chord);
    if (canon === null) return;
    indexes.set(canon, [...(indexes.get(canon) ?? []), i]);
  });
  for (const idx of indexes.values()) {
    if (isTapLongPair(idx.map((i) => entries[i]))) {
      for (const i of idx) flags[i] = true;
    }
  }
  return flags;
}

function groupByChord(entries: ChordEntry[]): Map<string, ChordEntry[]> {
  const groups = new Map<string, ChordEntry[]>();
  for (const entry of entries) {
    const canon = canonicalizeChord(entry.chord);
    if (canon === null) continue;
    groups.set(canon, [...(groups.get(canon) ?? []), entry]);
  }
  return groups;
}
