<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import {
    clipNow,
    isErrorReply,
    setRecorderEnabled,
    splitSession,
    startSession,
    stopSession,
    type Reply,
  } from '../daemon';
  import { getGameIcons } from '../api';
  import { describeChip, liveStatus, now, refreshConfig, refreshStatus, toasts } from '../status';
  import AppIcon from '../settings/fields/AppIcon.svelte';
  import { gateReason, NOTICE_MS, type GateAction } from './gates';
  import { chipKeys, startTopbarHotkeys, topbarHotkeys } from './hotkeys';
  import {
    OFF_CONFIRM_TEXT,
    needsOffConfirm,
    nextRecorderEnabled,
    switchDisabledReason,
    switchLabel,
  } from './recorderToggle';

  /**
   * Recorder state and quick actions for the left side of the title bar. The state chip, the
   * hotkey chips and the notices below the bar share one pending-action lock: one action runs at
   * a time.
   */

  interface Notice {
    id: number;
    kind: 'error' | 'warn';
    text: string;
  }

  const s = $derived($liveStatus);
  const chip = $derived(describeChip(s, $now));
  const hot = $derived($topbarHotkeys);

  /** Key of the action in flight (`split`, `start`, `stop`, `clip-<index>`), or null. */
  let pending = $state<string | null>(null);
  let notices = $state<Notice[]>([]);
  let noticeSeq = 0;

  /** Game icon as base64 PNG, or null for the letter tile. */
  let gameIcon = $state<string | null>(null);
  let iconFor = '';

  /** Recorder switch: disabled while offline or while any action runs. */
  const switchReason = $derived(switchDisabledReason(s, pending !== null));
  let switchEl = $state<HTMLButtonElement | undefined>(undefined);
  let popEl = $state<HTMLDivElement | undefined>(undefined);
  let cancelEl = $state<HTMLButtonElement | undefined>(undefined);
  let confirmOpen = $state(false);
  let popPos = $state({ left: 12, top: 42 });

  onMount(() => startTopbarHotkeys());

  // The confirm closes on its own when the session it asked about has already ended.
  $effect(() => {
    if (confirmOpen && !needsOffConfirm(s)) confirmOpen = false;
  });

  // Focus the safe choice (Cancel) when the confirm opens.
  $effect(() => {
    if (confirmOpen) cancelEl?.focus();
  });

  function notify(kind: Notice['kind'], text: string): void {
    noticeSeq += 1;
    const id = noticeSeq;
    notices = [...notices, { id, kind, text }];
    setTimeout(() => {
      notices = notices.filter((n) => n.id !== id);
    }, NOTICE_MS);
  }

  // A degraded recorder shows one notice when it degrades. The reason also stays in the tooltip.
  let shownDegraded: string | null = null;
  $effect(() => {
    const reason = s.degradedReason;
    if (reason !== null && reason !== shownDegraded) {
      untrack(() => notify('warn', `Recorder degraded: ${reason}`));
    }
    shownDegraded = reason;
  });

  // Fetch the icon of the attached game once per game id.
  $effect(() => {
    const id = s.game?.gameId ?? '';
    if (id === iconFor) return;
    iconFor = id;
    gameIcon = null;
    if (id === '') return;
    void getGameIcons([id]).then((found) => {
      if (iconFor === id) gameIcon = found[id] ?? null;
    });
  });

  /** Reason the action cannot run now, or null. */
  function blockedBy(action: GateAction): string | null {
    const gate = gateReason(s, action);
    if (gate !== null) return gate;
    if (pending !== null) return 'Another action is still running.';
    return null;
  }

  async function run(key: string, action: () => Promise<Reply>): Promise<void> {
    if (pending !== null) return;
    pending = key;
    try {
      const reply = await action();
      if (isErrorReply(reply)) {
        notify('error', reply.data.message);
        void refreshStatus();
      }
    } catch (e) {
      notify('error', e instanceof Error ? e.message : 'The request failed.');
      void refreshStatus();
    } finally {
      pending = null;
    }
  }

  function toggleSession(): void {
    if (s.session) {
      void run('stop', stopSession);
    } else {
      void run('start', startSession);
    }
  }

  /**
   * Sends the recorder on/off command. Turning it off saves a running session up to now. The
   * status and config are read back afterwards, so the switch shows what the daemon holds.
   */
  async function setRecorder(enabled: boolean): Promise<void> {
    if (pending !== null) return;
    pending = 'recorder';
    closeConfirm(false);
    try {
      const reply = await setRecorderEnabled(enabled);
      if (isErrorReply(reply)) notify('error', reply.data.message);
    } catch (e) {
      notify('error', e instanceof Error ? e.message : 'The request failed.');
    } finally {
      pending = null;
    }
    void refreshStatus();
    void refreshConfig();
  }

  function onSwitchClick(): void {
    if (needsOffConfirm(s)) {
      openConfirm();
      return;
    }
    void setRecorder(nextRecorderEnabled(s));
  }

  function openConfirm(): void {
    const r = switchEl?.getBoundingClientRect();
    popPos = { left: r?.left ?? 12, top: (r?.bottom ?? 36) + 6 };
    confirmOpen = true;
  }

  /** Closes the confirm. `refocus` returns focus to the switch (keyboard and Cancel paths). */
  function closeConfirm(refocus: boolean): void {
    if (!confirmOpen) return;
    confirmOpen = false;
    if (refocus) switchEl?.focus();
  }

  function onWindowKey(e: KeyboardEvent): void {
    if (confirmOpen && e.key === 'Escape') {
      e.preventDefault();
      closeConfirm(true);
    }
  }

  /** A press outside the confirm and the switch dismisses the confirm without an action. */
  function onWindowPointer(e: PointerEvent): void {
    if (!confirmOpen) return;
    const target = e.target as Node | null;
    if (target && (popEl?.contains(target) || switchEl?.contains(target))) return;
    closeConfirm(false);
  }

  const statusLabel = $derived(
    chip.tone === 'idle'
      ? 'Waiting For Game'
      : chip.tone === 'offline'
        ? 'Recorder offline'
        : chip.tone === 'recorder_off'
          ? 'Recorder off'
          : '',
  );

  const statusTooltip = $derived.by(() => {
    const lines = [chip.tooltip];
    if (s.connected && s.encoder && !chip.tooltip.includes('Encoder ')) {
      lines.push(`Encoder ${s.encoder}`);
    }
    if (s.degradedReason) lines.push(`Recorder degraded: ${s.degradedReason}`);
    if (s.settingsPendingRestart) lines.push('Settings apply when recording stops.');
    return lines.join('\n');
  });
</script>

{#snippet keyChord(chord: string)}
  {#if chord !== ''}
    <span class="keys" aria-hidden="true">{#each chipKeys(chord) as key, i (i)}<kbd>{key}</kbd>{/each}</span>
  {/if}
{/snippet}

<svelte:window onkeydown={onWindowKey} onpointerdown={onWindowPointer} />

<div class="recorder">
  <div class="bar">
    <button
      type="button"
      role="switch"
      class="switch"
      class:on={s.recorderEnabled}
      aria-checked={s.recorderEnabled ? 'true' : 'false'}
      aria-label={switchLabel(s.recorderEnabled)}
      title={switchReason ?? switchLabel(s.recorderEnabled)}
      disabled={switchReason !== null}
      bind:this={switchEl}
      onclick={onSwitchClick}
    >
      {#if pending === 'recorder'}<span class="spin" aria-hidden="true"></span>{/if}
      <span class="switch-label" aria-hidden="true">Recorder</span>
      <span class="track" aria-hidden="true"><span class="knob"></span></span>
    </button>

    <div class="status {chip.tone}" title={statusTooltip}>
      {#if s.game && s.connected}
        <AppIcon exe={s.game.exe} name={s.game.name} iconB64={gameIcon} size={18} />
      {:else}
        <span class="dot" class:pulse={chip.tone === 'recording'} aria-hidden="true"></span>
      {/if}
      {#if chip.tone === 'recording'}
        <span class="name">{chip.game}</span>
        <span class="clock">{chip.clock}</span>
      {:else if chip.tone === 'buffering'}
        <span class="sub">Buffering</span>
        <span class="name">{chip.game}</span>
        {#if chip.clock}<span class="clock">{chip.clock}</span>{/if}
      {:else}
        <span class="name idle">{statusLabel}</span>
      {/if}
    </div>

    {#if s.session}
      {@const reason = blockedBy('split')}
      <button
        type="button"
        class="chip"
        class:has-keys={hot.split !== ''}
        class:busy={pending === 'split'}
        disabled={reason !== null}
        title={reason ?? (hot.split ? `Split the session into a new part (${hot.split})` : 'Split the session into a new part')}
        aria-label="Split session"
        onclick={() => void run('split', splitSession)}
      >
        {#if pending === 'split'}<span class="spin" aria-hidden="true"></span>{/if}
        {@render keyChord(hot.split)}
        <span class="label">Split</span>
      </button>
    {/if}

    {#if s.session}
      {@const reason = blockedBy('stop')}
      <button
        type="button"
        class="chip danger"
        class:has-keys={hot.startStop !== ''}
        class:busy={pending === 'stop'}
        disabled={reason !== null}
        title={reason ?? (hot.startStop ? `Stop the session (${hot.startStop})` : 'Stop the session')}
        aria-label="Stop session"
        onclick={toggleSession}
      >
        {#if pending === 'stop'}<span class="spin" aria-hidden="true"></span>{/if}
        {@render keyChord(hot.startStop)}
        <span class="label">Stop session</span>
      </button>
    {:else}
      {@const reason = blockedBy('start')}
      <button
        type="button"
        class="chip"
        class:has-keys={hot.startStop !== ''}
        class:busy={pending === 'start'}
        disabled={reason !== null}
        title={reason ?? (hot.startStop ? `Start a session (${hot.startStop})` : 'Start a session')}
        aria-label="Start session"
        onclick={toggleSession}
      >
        {#if pending === 'start'}<span class="spin" aria-hidden="true"></span>{/if}
        {@render keyChord(hot.startStop)}
        <span class="label">Start session</span>
      </button>
    {/if}

    {#each hot.clips as clip, i (i)}
      {@const key = `clip-${i}`}
      {@const reason = blockedBy('clip')}
      {@const long = clip.press === 'long'}

      <button
        type="button"
        class="chip opt"
        class:has-keys={clip.keys !== ''}
        class:busy={pending === key}
        class:long
        disabled={reason !== null}
        title={reason ?? `Save the last ${clip.seconds} seconds (${clip.name}${long ? ', long press' : ''})`}
        aria-label={`Save clip: ${clip.name}, last ${clip.seconds} seconds${long ? ', long press' : ''}`}
        onclick={() => void run(key, () => clipNow(clip.seconds))}
      >
        {#if pending === key}<span class="spin" aria-hidden="true"></span>{/if}
        {@render keyChord(clip.keys)}
        {#if long}<span class="hold" aria-hidden="true">hold</span>{/if}
        <span class="label">{clip.name}</span>
      </button>
    {/each}
  </div>

  {#if confirmOpen}
    <div
      class="confirm"
      role="dialog"
      aria-labelledby="recorder-confirm-text"
      bind:this={popEl}
      style:left="{popPos.left}px"
      style:top="{popPos.top}px"
    >
      <p id="recorder-confirm-text">{OFF_CONFIRM_TEXT}</p>
      <div class="confirm-actions">
        <button type="button" class="chip danger" disabled={pending !== null} onclick={() => void setRecorder(false)}>
          Turn off
        </button>
        <button type="button" class="chip" bind:this={cancelEl} onclick={() => closeConfirm(true)}>Cancel</button>
      </div>
    </div>
  {/if}

  {#if notices.length > 0 || $toasts.length > 0}
    <div class="notices">
      {#each notices as n (n.id)}
        <div class="notice {n.kind}" role={n.kind === 'error' ? 'alert' : 'status'}>{n.text}</div>
      {/each}
      {#each $toasts as t (t.id)}
        <div class="notice info" role="status">{t.text}</div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .recorder {
    display: flex;
    align-items: center;
    min-width: 0;
    height: 100%;
  }

  .bar {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
    overflow: hidden;
  }

  .status {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex: 0 1 auto;
    min-width: 0;
    max-width: 260px;
    padding-right: 6px;
    font-size: 12px;
    white-space: nowrap;
    color: var(--text);
    cursor: default;
  }

  .status.idle .name,
  .status.offline .name,
  .status.recorder_off .name {
    color: var(--text-dim);
  }

  /* Recorder switch: a compact toggle in the style of the top bar's chips. */
  .switch {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    flex: 0 0 auto;
    height: 24px;
    padding: 0 8px;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 12px;
    white-space: nowrap;
    cursor: pointer;
  }

  .switch:hover:not(:disabled) {
    border-color: var(--accent);
  }

  .switch:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .switch:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .switch .track {
    position: relative;
    flex: 0 0 auto;
    width: 26px;
    height: 14px;
    border-radius: 7px;
    background: var(--border);
    transition: background 0.15s ease;
  }

  .switch .knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--text-dim);
    transition: transform 0.15s ease;
  }

  .switch.on .track {
    background: var(--accent);
  }

  .switch.on .knob {
    background: #fff;
    transform: translateX(12px);
  }

  /* Confirm shown before turning the recorder off while a session records. Fixed, so the bar's overflow does not clip it. */
  .confirm {
    position: fixed;
    z-index: 30;
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-width: min(320px, calc(100vw - 24px));
    padding: 10px 12px;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.25);
    font-size: 13px;
  }

  .confirm p {
    margin: 0;
    line-height: 1.4;
  }

  .confirm-actions {
    display: flex;
    gap: 6px;
    justify-content: flex-end;
  }

  .status.offline .name {
    color: var(--danger);
  }

  .dot {
    flex: 0 0 auto;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--text-dim);
  }

  .status.recording .dot {
    background: #e5484d;
  }

  .status.buffering .dot {
    background: #f5a524;
  }

  .status.offline .dot {
    background: var(--danger);
  }

  .dot.pulse {
    animation: pulse 1.6s ease-in-out infinite;
  }

  @keyframes pulse {
    0%,
    100% {
      box-shadow: 0 0 0 0 rgba(229, 72, 77, 0.6);
    }
    50% {
      box-shadow: 0 0 0 5px rgba(229, 72, 77, 0);
    }
  }

  .sub {
    flex: 0 0 auto;
    color: var(--text-dim);
  }

  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    font-weight: 600;
  }

  .clock {
    flex: 0 0 auto;
    font-variant-numeric: tabular-nums;
    color: var(--text-dim);
  }

  .chip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    flex: 0 0 auto;
    height: 24px;
    max-width: 200px;
    padding: 0 8px;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 12px;
    white-space: nowrap;
    cursor: pointer;
  }

  .chip:hover:not(:disabled) {
    border-color: var(--accent);
  }

  .chip.danger {
    border-color: #e5484d;
  }

  .chip:disabled {
    opacity: 0.5;
    cursor: default;
  }

  .chip:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }

  .chip .label {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .keys {
    display: inline-flex;
    gap: 3px;
    flex: 0 0 auto;
  }

  /* Marks a clip that fires on long press, so it can share a chord with a tap clip. */
  .hold {
    flex: 0 0 auto;
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: lowercase;
    color: var(--text-dim);
  }

  kbd {
    padding: 0 4px;
    min-width: 16px;
    text-align: center;
    font-family: inherit;
    font-size: 10px;
    font-weight: 700;
    line-height: 16px;
    letter-spacing: 0.02em;
    text-transform: uppercase;
    color: var(--text);
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: 4px;
  }

  .spin {
    flex: 0 0 auto;
    width: 10px;
    height: 10px;
    border: 2px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .dot.pulse {
      animation: none;
    }

    .switch .track,
    .switch .knob {
      transition: none;
    }

    .spin {
      animation-duration: 2.4s;
    }
  }

  /* Narrow window: chips keep their keys and drop the words. The switch keeps its track. */
  @media (max-width: 760px) {
    .chip.has-keys .label,
    .chip.has-keys .hold,
    .switch-label {
      display: none;
    }

    .sub {
      display: none;
    }
  }

  /* Very narrow: only the status and the session chips remain. */
  @media (max-width: 560px) {
    .chip.opt {
      display: none;
    }
  }

  /* Notices drop below the title bar and never take layout space. */
  .notices {
    position: absolute;
    top: calc(100% + 6px);
    left: 12px;
    z-index: 20;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
    max-width: min(420px, calc(100vw - 24px));
    pointer-events: none;
  }

  .notice {
    padding: 7px 10px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 13px;
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.25);
  }

  .notice.error {
    color: var(--danger);
    border-color: var(--danger);
  }

  .notice.warn {
    color: #f5a524;
    border-color: #f5a524;
  }

  .notice.info {
    border-color: var(--accent);
  }
</style>
