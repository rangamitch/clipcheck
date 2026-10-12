<script lang="ts">
  import FieldRow from '../fields/FieldRow.svelte';
  import KeyCapture from '../fields/KeyCapture.svelte';
  import { pressOf } from '../clips';
  import {
    canAddPushToTalkKey,
    chordLabel,
    chordProblem,
    findDuplicateChords,
    MAX_PUSH_TO_TALK_KEYS,
    tapLongPairFlags,
    type ChordEntry,
  } from '../hotkeys';
  import { settings, setPath } from '../store';

  interface Props {
    disabled?: boolean;
  }

  let { disabled = false }: Props = $props();

  const hotkeys = $derived($settings.draft?.hotkeys);
  const session = $derived(hotkeys?.session);
  const diagnostics = $derived($settings.diagnostics);

  const SESSION_KEYS: { key: 'split' | 'stop_start' | 'toggle_buffer' | 'mute_mic'; label: string; help: string }[] = [
    { key: 'split', label: 'Split the session', help: 'Starts a new part of the same session. Default Alt+F11.' },
    { key: 'stop_start', label: 'Start or stop the session', help: 'Default Alt+F12.' },
    { key: 'toggle_buffer', label: 'Turn the replay buffer on or off', help: 'Default Alt+F8.' },
    {
      key: 'mute_mic',
      label: 'Mute or unmute the microphone (toggle)',
      help: 'Default Alt+F7. Clear it to turn this off.',
    },
  ];

  /** Push-to-talk keys in the draft. Empty when the session config has none yet. */
  const pttKeys = $derived<string[]>(session?.push_to_talk ?? []);
  /** Legacy: the mute key still holds to talk (`mute_mic_mode = "push_to_talk"`). */
  const legacyMuteHold = $derived(session?.mute_mic_mode === 'push_to_talk');
  /**
   * True while the user has pressed "Add" and not yet captured a key. The empty row is kept out of
   * the draft, because an empty chord is not a valid config value.
   */
  let addingPtt = $state(false);
  const canAddPtt = $derived(canAddPushToTalkKey(pttKeys.length) && !addingPtt);

  /** Every chord in use: clip hotkeys, session hotkeys and each push-to-talk key. */
  const entries = $derived<ChordEntry[]>(
    hotkeys
      ? [
          ...hotkeys.clips.map((c, i) => ({
            label: `${c.name.trim() === '' ? `clip hotkey ${i + 1}` : `"${c.name}"`} (${pressOf(c) === 'long' ? 'long press' : 'tap'})`,
            chord: c.keys,
            press: pressOf(c),
          })),
          ...SESSION_KEYS.map((k) => ({ label: k.label.toLowerCase(), chord: session?.[k.key] ?? '' })),
          ...pttKeys.map((chord, i) => ({ label: `push to talk key ${i + 1}`, chord })),
        ]
      : [],
  );

  const duplicates = $derived(findDuplicateChords(entries));
  /** Chords that hold a Tap + Long press pair. Allowed, but shown so the user sees both lengths. */
  const pairChords = $derived(
    [...new Set(entries.filter((e, i) => e.press === 'tap' && tapLongPairFlags(entries)[i]).map((e) => e.chord))],
  );
  /** Chords that cannot be bound, such as a bare letter or a Windows-reserved combination. */
  const problems = $derived(
    entries
      .filter((e) => e.chord !== '')
      .map((e) => ({ label: e.label, chord: e.chord, problem: chordProblem(e.chord) }))
      .filter((p) => p.problem !== null),
  );
  /** Findings from the recorder that belong to this tab, shown once here as well as per field. */
  const recorderNotes = $derived(
    diagnostics.filter((d) => d.path.startsWith('hotkeys')).map((d) => ({ key: `${d.code}-${d.path}`, ...d })),
  );
  const hasConflicts = $derived(duplicates.length > 0 || problems.length > 0);

  const PROBLEM_TEXT: Record<'needs_modifier' | 'reserved' | 'invalid', string> = {
    needs_modifier: 'needs Ctrl, Alt, Shift or Win (function, navigation, lock, media and volume keys can be used alone)',
    reserved: 'is reserved by Windows or ClipCheck',
    invalid: 'is not a valid key combination',
  };

  /** Writes the whole push-to-talk list. Arrays are sent whole, as clips and audio apps are. */
  function setPttKeys(keys: string[]): void {
    setPath('hotkeys.session.push_to_talk', keys);
  }

  function changePttKey(index: number, chord: string): void {
    setPttKeys(pttKeys.map((k, i) => (i === index ? chord : k)));
  }

  function removePttKey(index: number): void {
    setPttKeys(pttKeys.filter((_, i) => i !== index));
  }

  /** The captured key for the pending row joins the list. The list never holds an empty chord. */
  function addCapturedPttKey(chord: string): void {
    addingPtt = false;
    if (chord === '' || !canAddPushToTalkKey(pttKeys.length)) return;
    setPttKeys([...pttKeys, chord]);
  }

  /** Switches the legacy mute-key hold mode to toggle. */
  function switchLegacyToToggle(): void {
    setPath('hotkeys.session.mute_mic_mode', 'toggle');
  }
</script>

{#if hotkeys && session}
  <h2>Hotkeys</h2>
  <p class="intro">
    These keys work while you play. Click a box, then press the key or shortcut. Esc cancels. Clip
    hotkeys are set on the Recordings tab. Win+ combinations may be reserved by Windows.
  </p>

  {#each SESSION_KEYS as item (item.key)}
    <FieldRow label={item.label} path={`hotkeys.session.${item.key}`} help={item.help}>
      {#snippet children(ctl)}
        <KeyCapture
          {...ctl}
          value={session[item.key]}
          allowClear={true}
          {disabled}
          onchange={(v) => setPath(`hotkeys.session.${item.key}`, v)}
        />
      {/snippet}
    </FieldRow>
  {/each}

  <section class="ptt" aria-labelledby="ptt-heading">
    <h3 id="ptt-heading">Push to talk</h3>
    <p class="intro">
      Hold a key or mouse button to talk. Your microphone only records while it is held. Add more than
      one key if you want several ways to talk; they all use the same track.
    </p>

    {#if legacyMuteHold}
      <p class="legacy">
        The mute key is currently set to hold-to-talk (older setting).
        <button type="button" class="link" {disabled} onclick={switchLegacyToToggle}>Switch it to toggle</button>
      </p>
    {/if}

    <ul class="ptt-keys">
      {#each pttKeys as chord, i (i)}
        <li class="ptt-row">
          <span class="ptt-label" id={`ptt-key-${i}`}>Push-to-talk key {i + 1}</span>
          <KeyCapture
            value={chord}
            allowMouse={true}
            {disabled}
            labelId={`ptt-key-${i}`}
            onchange={(v) => changePttKey(i, v)}
          />
          <button type="button" class="remove" {disabled} onclick={() => removePttKey(i)}>
            Remove<span class="sr-only"> push-to-talk key {i + 1}</span>
          </button>
        </li>
      {/each}
      {#if addingPtt}
        <li class="ptt-row">
          <span class="ptt-label" id="ptt-key-new">New push-to-talk key</span>
          <KeyCapture
            value=""
            allowMouse={true}
            {disabled}
            labelId="ptt-key-new"
            onchange={addCapturedPttKey}
          />
          <button type="button" class="remove" {disabled} onclick={() => (addingPtt = false)}>Cancel</button>
        </li>
      {/if}
    </ul>

    <div class="ptt-actions">
      <button type="button" class="add" disabled={disabled || !canAddPtt} onclick={() => (addingPtt = true)}>
        Add push-to-talk key
      </button>
      <span class="count">{pttKeys.length} of {MAX_PUSH_TO_TALK_KEYS} used</span>
    </div>

    {#if pttKeys.length > 0}
      <p class="note">
        Push to talk uses a keyboard hook that watches each key while the game has focus. Mouse buttons
        use a mouse hook that is only active while a mouse button is bound. Some anti-cheat systems may
        flag these hooks, so leave push to talk off if you are unsure.
      </p>
    {/if}
  </section>

  <section class="conflicts" aria-labelledby="conflicts-heading" aria-live="polite">
    <h3 id="conflicts-heading">Conflicts</h3>
    {#each pairChords as chord (chord)}
      <p class="ok">
        <strong>{chordLabel(chord)}</strong> saves two lengths: Tap and Long press. This is allowed.
      </p>
    {/each}
    {#if !hasConflicts}
      <p class="ok">No conflicts. Every shortcut is different.</p>
    {:else}
      <ul>
        {#each duplicates as dup (dup.chord)}
          <li class="bad">
            <strong>{chordLabel(dup.chord)}</strong> is used by {dup.labels.join(' and ')}. Give each one its own shortcut.
          </li>
        {/each}
        {#each problems as p (p.label + p.chord)}
          <li class="bad">
            <strong>{chordLabel(p.chord)}</strong> for {p.label} {PROBLEM_TEXT[p.problem ?? 'invalid']}.
          </li>
        {/each}
      </ul>
    {/if}

    {#if recorderNotes.length > 0}
      <h4>From the recorder</h4>
      <ul>
        {#each recorderNotes as note (note.key)}
          <li class={note.severity === 'error' ? 'bad' : 'warn'}>
            {note.message}
          </li>
        {/each}
      </ul>
    {/if}
  </section>
{/if}

<style>
  h3 {
    margin: 0 0 6px;
  }

  h4 {
    margin: 12px 0 4px;
    font-size: 13px;
    color: var(--text-dim);
  }

  .intro,
  .ok {
    margin: 0 0 8px;
    font-size: 13px;
    color: var(--text-dim);
  }

  .note {
    margin: 0;
    font-size: 13px;
    color: var(--text-dim);
    max-width: 640px;
  }

  .conflicts {
    margin-top: 22px;
    padding: 12px 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface);
  }

  ul {
    margin: 0;
    padding-left: 18px;
    display: flex;
    flex-direction: column;
    gap: 4px;
    font-size: 13px;
  }

  li.bad {
    color: var(--danger);
  }

  li.warn {
    color: var(--text-dim);
  }

  .ptt {
    margin-top: 22px;
    padding-top: 14px;
    border-top: 1px solid var(--border);
  }

  .ptt-keys {
    padding-left: 0;
    list-style: none;
    gap: 10px;
    margin: 10px 0;
  }

  .ptt-row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 12px;
  }

  .ptt-label {
    min-width: 170px;
    font-size: 14px;
  }

  .ptt-actions {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .legacy {
    margin: 0 0 8px;
    font-size: 13px;
    color: var(--text-dim);
  }

  .count {
    font-size: 13px;
    color: var(--text-dim);
  }

  button.add,
  button.remove,
  button.link {
    font: inherit;
    font-size: 13px;
    padding: 5px 10px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    color: var(--text);
    cursor: pointer;
  }

  button.link {
    padding: 0;
    border: none;
    background: none;
    color: var(--accent);
    text-decoration: underline;
  }

  button:disabled {
    opacity: 0.5;
    cursor: not-allowed;
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
