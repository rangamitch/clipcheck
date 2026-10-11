<script lang="ts">
  import { onDestroy } from 'svelte';
  import {
    chordLabel,
    eventToChord,
    heldModifiersLabel,
    isReservedChord,
    modifiersOf,
  } from '../hotkeys';
  import { beginHotkeyCapture } from '../hotkeySuspend';

  interface Props {
    /** Canonical chord, or "" when unbound. */
    value: string;
    /** Whether Backspace or Delete may clear the binding (session hotkeys only). */
    allowClear?: boolean;
    disabled?: boolean;
    id?: string;
    labelId?: string;
    describedBy?: string;
    onchange: (chord: string) => void;
  }

  let {
    value,
    allowClear = false,
    disabled = false,
    id,
    labelId,
    describedBy,
    onchange,
  }: Props = $props();

  let listening = $state(false);
  /** Modifiers held right now, shown while listening. */
  let held = $state('');
  /** Why the last key press was refused, shown under the button. */
  let problem = $state<string | null>(null);
  /** Text for the polite live region. Changes at each step of capture. */
  let announce = $state('');
  /** True when the last keydown was captured here, so its keyup must not reach the button or window. */
  let keyDownCaptured = false;
  /** Releases this box's hold on the daemon's global hotkeys. Null when not listening. */
  let releaseHotkeys: (() => void) | null = null;

  const shown = $derived(value === '' ? 'Not set' : chordLabel(value));

  function start(): void {
    // A click while already listening is ignored, so the capture is not restarted by the mouse.
    if (disabled || listening) return;
    listening = true;
    held = '';
    problem = null;
    // Pause the daemon's global hotkeys, so the chord being recorded does not fire a clip.
    releaseHotkeys ??= beginHotkeyCapture();
    announce = 'Listening. Press a key or shortcut, or Escape to cancel.';
  }

  function stop(): void {
    listening = false;
    held = '';
    releaseHotkeys?.();
    releaseHotkeys = null;
  }

  // Tab switch, window hide and page hide end the capture, as blur does.
  $effect(() => {
    if (!listening) return;
    const hide = (): void => cancel();
    const onVisibility = (): void => {
      if (document.visibilityState === 'hidden') cancel();
    };
    window.addEventListener('pagehide', hide);
    document.addEventListener('visibilitychange', onVisibility);
    return () => {
      window.removeEventListener('pagehide', hide);
      document.removeEventListener('visibilitychange', onVisibility);
    };
  });

  // Component destroyed mid-capture: the daemon must not stay paused.
  onDestroy(() => {
    releaseHotkeys?.();
    releaseHotkeys = null;
  });

  function cancel(): void {
    if (!listening) return;
    stop();
    problem = null;
    announce = 'Cancelled. The shortcut is unchanged.';
  }

  function commit(chord: string): void {
    stop();
    problem = null;
    announce = chord === '' ? 'Shortcut cleared.' : `Shortcut set to ${chordLabel(chord)}.`;
    onchange(chord);
  }

  function refuse(message: string): void {
    problem = message;
    announce = message;
  }

  function onkeydown(e: KeyboardEvent): void {
    if (!listening) return;
    // Every key belongs to the capture while it runs: Tab, Backspace, F5 and Ctrl+S do not act
    // in the page or the window, and Space or Enter do not click the button.
    e.preventDefault();
    e.stopPropagation();
    keyDownCaptured = true;
    if (e.key === 'Escape' || e.code === 'Escape') {
      cancel();
      return;
    }
    const mods = modifiersOf(e);
    const anyMod = mods.ctrl || mods.alt || mods.shift || mods.win;
    if (allowClear && !anyMod && (e.key === 'Backspace' || e.key === 'Delete')) {
      commit('');
      return;
    }
    held = heldModifiersLabel(mods);
    const result = eventToChord(e);
    if (!result.ok) {
      if (result.reason === 'modifier_only') {
        announce = held ? `Holding ${held}.` : '';
        return;
      }
      if (result.reason === 'unsupported_key') {
        refuse('That key cannot be used as a shortcut. Try another key, or Esc to cancel.');
        return;
      }
      refuse(
        'Add Ctrl, Alt, Shift or Win to this key. Only function, navigation, lock, media and volume keys can be used alone.',
      );
      return;
    }
    const reserved = isReservedChord(result.chord);
    if (reserved) {
      refuse(`${chordLabel(result.chord)} is reserved by Windows: ${reserved}.`);
      return;
    }
    commit(result.chord);
  }

  function onkeyup(e: KeyboardEvent): void {
    const captured = keyDownCaptured;
    keyDownCaptured = false;
    if (captured || listening) {
      e.preventDefault();
      e.stopPropagation();
    }
    if (!listening) return;
    held = heldModifiersLabel(modifiersOf(e));
  }
</script>

<div class="capture">
  <button
    type="button"
    class="key"
    class:listening
    class:unset={value === ''}
    class:invalid={problem !== null}
    {id}
    {disabled}
    aria-labelledby={labelId}
    aria-describedby={describedBy}
    onclick={start}
    onblur={cancel}
    onkeydown={onkeydown}
    onkeyup={onkeyup}
  >
    {#if listening}
      <span class="hint">{held || 'Press a key or shortcut'}</span>
    {:else}
      <span class="chord">{shown}</span>
    {/if}
  </button>
  {#if listening}
    <span class="sub">Esc cancels{allowClear ? ', Backspace or Delete clears' : ''}. Clicking elsewhere cancels.</span>
  {/if}
  {#if problem}
    <p class="problem" role="alert">{problem}</p>
  {/if}
  <span class="sr-only" aria-live="polite">{announce}</span>
</div>

<style>
  .capture {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
  }

  .key {
    min-width: 200px;
    padding: 7px 12px;
    border-radius: var(--radius);
    border: 1px dashed var(--border);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
    text-align: left;
    cursor: pointer;
  }

  .key.unset .chord {
    color: var(--text-dim);
  }

  .chord {
    font-family: ui-monospace, Consolas, monospace;
  }

  .key.listening {
    border-style: solid;
    border-color: var(--accent);
  }

  .hint {
    color: var(--text-dim);
  }

  .key.invalid {
    border-color: var(--danger);
  }

  .key:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .key:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .sub {
    font-size: 12px;
    color: var(--text-dim);
  }

  .problem {
    flex: 0 0 100%;
    margin: 0;
    font-size: 13px;
    color: var(--danger);
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
</style>
