<script lang="ts">
  import FieldRow from '../fields/FieldRow.svelte';
  import KeyCapture from '../fields/KeyCapture.svelte';
  import Segmented from '../fields/Segmented.svelte';
  import { pressOf } from '../clips';
  import { chordLabel, chordProblem, findDuplicateChords, tapLongPairFlags, type ChordEntry } from '../hotkeys';
  import { settings, setPath, type MuteMode } from '../store';

  interface Props {
    disabled?: boolean;
  }

  let { disabled = false }: Props = $props();

  const hotkeys = $derived($settings.draft?.hotkeys);
  const session = $derived(hotkeys?.session);
  const diagnostics = $derived($settings.diagnostics);

  const MODE_OPTIONS: { value: MuteMode; label: string }[] = [
    { value: 'toggle', label: 'Press to toggle' },
    { value: 'push_to_talk', label: 'Hold to talk (push to talk)' },
  ];

  const SESSION_KEYS: { key: 'split' | 'stop_start' | 'toggle_buffer' | 'mute_mic'; label: string; help: string }[] = [
    { key: 'split', label: 'Split the session', help: 'Starts a new part of the same session. Default Alt+F11.' },
    { key: 'stop_start', label: 'Start or stop the session', help: 'Default Alt+F12.' },
    { key: 'toggle_buffer', label: 'Turn the replay buffer on or off', help: 'Default Alt+F8.' },
    { key: 'mute_mic', label: 'Mute or unmute the microphone', help: 'Default Alt+F7. Clear it to turn this off.' },
  ];

  /** Every chord in use, clip hotkeys and session hotkeys together. */
  const entries = $derived<ChordEntry[]>(
    hotkeys
      ? [
          ...hotkeys.clips.map((c, i) => ({
            label: `${c.name.trim() === '' ? `clip hotkey ${i + 1}` : `"${c.name}"`} (${pressOf(c) === 'long' ? 'long press' : 'tap'})`,
            chord: c.keys,
            press: pressOf(c),
          })),
          ...SESSION_KEYS.map((k) => ({ label: k.label.toLowerCase(), chord: session?.[k.key] ?? '' })),
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

  <FieldRow
    label="Microphone mute style"
    path="hotkeys.session.mute_mic_mode"
    help="Press to toggle switches the mic on or off with each press. Hold to talk mutes the mic until you let go."
  >
    {#snippet children(ctl)}
      <Segmented
        {...ctl}
        name="mute-mic-mode"
        value={session.mute_mic_mode}
        options={MODE_OPTIONS}
        {disabled}
        onchange={(v) => setPath('hotkeys.session.mute_mic_mode', v)}
      />
      {#if session.mute_mic_mode === 'push_to_talk'}
        <p class="note">
          Hold to talk needs a keyboard hook that watches the key while the game has focus. Some
          anti-cheat systems may flag this, so use toggle if you are unsure.
        </p>
      {/if}
    {/snippet}
  </FieldRow>

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
</style>
