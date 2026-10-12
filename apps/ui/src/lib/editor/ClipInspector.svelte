<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { get } from 'svelte/store';
  import { editorApi } from './api';
  import { dispatch, dispatchWith, playhead, project, toast } from './store';
  import { clipEnd, formatTimecode, secToTick, srcLen, tickToSec } from './ticks';
  import {
    defaultTransform,
    type Clip,
    type ClipPatch,
    type Command,
    type Crop,
    type FadeCurve,
    type FitMode,
    type LoudnessReport,
    type MediaRef,
    type Project,
    type Rate,
    type Rgba,
    type TextSpec,
    type Tick,
    type Track,
    type Transform,
    type TransitionKind,
  } from './types';

  interface Props {
    /** The selected clips (at least one). Each section shows the clips it applies to. */
    clips: Clip[];
  }

  let { clips }: Props = $props();

  /** Idle time before a slider edit is committed, in milliseconds. */
  const IDLE_MS = 250;
  /** Idle time before typed title text is committed, in milliseconds. */
  const TEXT_IDLE_MS = 300;
  /** Delay value meaning "commit now". */
  const NOW = 0;

  const FONTS = ['Segoe UI', 'Arial', 'Impact', 'Consolas', 'Times New Roman', 'Georgia', 'Verdana'];
  const LABEL_COLOURS = [
    '#ed1c24',
    '#f59e0b',
    '#facc15',
    '#22c55e',
    '#06b6d4',
    '#3b82f6',
    '#a855f7',
    '#ec4899',
  ];
  const SPEED_PRESETS = [0.25, 0.5, 1, 2, 4];
  const SPEED_MIN = 0.1;
  const SPEED_MAX = 8;
  const GAIN_MIN = -60;
  const GAIN_MAX = 12;
  const TEXT_SIZE_MIN = 8;
  const TEXT_SIZE_MAX = 300;
  const CROP_SIDES: { side: keyof Crop; label: string }[] = [
    { side: 'left', label: 'Left' },
    { side: 'top', label: 'Top' },
    { side: 'right', label: 'Right' },
    { side: 'bottom', label: 'Bottom' },
  ];
  /** Largest single crop side, as a fraction of the frame. */
  const CROP_MAX = 0.9;
  /** Largest sum of opposite crop sides (the backend requires a sum below 0.95). */
  const CROP_SUM_MAX = 0.94;
  const OPPOSITE_SIDE: Record<keyof Crop, keyof Crop> = {
    left: 'right',
    right: 'left',
    top: 'bottom',
    bottom: 'top',
  };
  const FIT_MODES: FitMode[] = ['fit', 'fill', 'stretch', 'custom'];
  const FIT_LABELS: Record<FitMode, string> = {
    fit: 'Fit',
    fill: 'Fill',
    stretch: 'Stretch',
    custom: 'Custom',
  };
  const FADE_CURVES: { value: FadeCurve; label: string }[] = [
    { value: 'linear', label: 'Linear' },
    { value: 'equal_power', label: 'Equal power' },
  ];
  const ALIGNS: { value: string; label: string }[] = [
    { value: 'left', label: 'Left' },
    { value: 'center', label: 'Centre' },
    { value: 'right', label: 'Right' },
  ];
  type TransitionChoice = TransitionKind | 'none';
  const TRANSITION_CHOICES: { value: TransitionChoice; label: string }[] = [
    { value: 'none', label: 'None' },
    { value: 'crossfade', label: 'Crossfade' },
    { value: 'dip_to_black', label: 'Dip to black' },
    { value: 'dip_to_white', label: 'Dip to white' },
  ];
  const DEFAULT_TRANSITION_S = 1;
  const MIN_TRANSITION_S = 0.1;
  const MAX_TRANSITION_S = 5;
  /** Clips shorter than this cannot take a new transition. */
  const MIN_CLIP_FOR_TRANSITION_S = 0.2;
  /** Length of a freeze frame inserted from the Actions row. */
  const FREEZE_S = 2;
  /** Target peak level for normalising, in dBFS. */
  const PEAK_TARGET_DB = -1;
  /** Measured peaks at or below this level are treated as silence and never normalised. */
  const SILENT_PEAK_DB = -100;
  /** Alpha used when a text box background is switched on. */
  const BOX_ALPHA = 160;
  /** Default colour for a new outline or background box. */
  const DEFAULT_DARK_HEX = '#000000';

  /** Normalising goal: peak level, or integrated loudness in LUFS. */
  type NormGoal = { kind: 'peak' } | { kind: 'lufs'; target: number };

  // ---------------------------------------------------------------------------------------------
  // Derived selection state
  // ---------------------------------------------------------------------------------------------

  const proj = $derived($project);
  const rate: Rate = $derived(proj?.canvas.fps ?? { num: 30, den: 1 });
  const mediaById = $derived(
    new Map<string, MediaRef>((proj?.media ?? []).map((m): [string, MediaRef] => [m.id, m])),
  );
  const trackById = $derived(
    new Map<string, Track>((proj?.tracks ?? []).map((t): [string, Track] => [t.id, t])),
  );
  const ids = $derived(clips.map((c) => c.id));

  // Per-kind subsets. Each section applies its patches only to the ids of its own subset.
  const visualClips = $derived(clips.filter((c) => c.kind !== 'audio'));
  const visualIds = $derived(visualClips.map((c) => c.id));
  const audioClips = $derived(clips.filter((c) => c.kind === 'audio'));
  const audioIds = $derived(audioClips.map((c) => c.id));
  const titleClips = $derived(clips.filter((c) => c.kind === 'title'));
  const titleIds = $derived(titleClips.map((c) => c.id));
  const solidClips = $derived(clips.filter((c) => c.kind === 'solid'));
  const solidIds = $derived(solidClips.map((c) => c.id));
  const speedClips = $derived(
    clips.filter((c) => (c.kind === 'video' || c.kind === 'audio') && c.speed > 0),
  );
  const speedIds = $derived(speedClips.map((c) => c.id));
  const videoClips = $derived(clips.filter((c) => c.kind === 'video'));
  const videoIds = $derived(videoClips.map((c) => c.id));
  const freezeTargets = $derived(
    videoClips.filter((c) => $playhead > c.start && $playhead < clipEnd(c)),
  );
  const envelopeClips = $derived(audioClips.filter((c) => c.audio.envelope.length > 0));
  const envelopeIds = $derived(envelopeClips.map((c) => c.id));

  /** True when any selected clip sits on a locked track; property edits are then disabled. */
  const anyLocked = $derived(clips.some((c) => trackById.get(c.track_id)?.locked === true));

  const firstLink = $derived(clips[0]?.link_id ?? null);
  const allLinked = $derived(
    firstLink !== null && clips.every((c) => c.link_id !== null && c.link_id === firstLink),
  );
  const firstTitle = $derived(titleClips[0]?.text ?? null);
  const firstSolid = $derived(solidClips[0]?.solid ?? null);
  const titleText = $derived(shared(titleClips.map((c) => c.text?.text ?? '')));
  const names = $derived(shared(clips.map((c) => c.name)));
  const colours = $derived(shared(clips.map((c) => c.color)));
  const enabledState = $derived(shared(clips.map((c) => c.enabled)));
  const speedState = $derived(shared(speedClips.map((c) => c.speed)));
  const reverseState = $derived(shared(speedClips.map((c) => c.reverse)));
  const mediaText = $derived.by((): string => {
    const s = shared(clips.map(mediaLabel));
    return s.mixed ? 'Mixed' : (s.value ?? '');
  });
  const panText = $derived.by((): string => {
    const s = shared(audioClips.map((c) => c.audio.pan));
    if (s.mixed) return 'Mixed';
    const p = Math.round((s.value ?? 0) * 100);
    if (p === 0) return 'Centre';
    return p < 0 ? `${-p}% L` : `${p}% R`;
  });

  /** Audio stream choices, labelled from the first audio clip's media. */
  const streamChoices = $derived.by((): { value: number; label: string }[] => {
    const first = audioClips[0];
    const media = first?.media_id ? mediaById.get(first.media_id) : undefined;
    const count = Math.max(1, media?.audio.length ?? 1);
    const out: { value: number; label: string }[] = [];
    for (let i = 0; i < count; i++) {
      const a = media?.audio[i];
      out.push({ value: i, label: a && a.title.length > 0 ? a.title : `Stream ${i + 1}` });
    }
    return out;
  });

  /** Transition state of the visual clips. */
  const transitionState = $derived(shared(visualClips.map(transitionChoice)));
  const anyTransition = $derived(visualClips.some((c) => c.transition_in !== null));
  /** Whether a new transition can be started on at least one visual clip. */
  const creatable = $derived(proj !== null && visualClips.some((c) => canStartTransition(proj, c)));
  /** Whether the transition kind can be chosen at all (new ones, or changing existing ones). */
  const transitionChangeable = $derived(creatable || anyTransition);
  /** Some visual clip cannot take a new transition, so the note explains why. */
  const transitionNote = $derived(
    proj !== null &&
      visualClips.some((c) => c.transition_in === null && !canStartTransition(proj, c)),
  );

  // ---------------------------------------------------------------------------------------------
  // Pure helpers
  // ---------------------------------------------------------------------------------------------

  /** Clamps `v` into `lo..hi`. */
  function clamp(v: number, lo: number, hi: number): number {
    return Math.min(hi, Math.max(lo, v));
  }

  /** Rounds `v` to `places` decimal places. */
  function round(v: number, places: number): number {
    const f = 10 ** places;
    return Math.round(v * f) / f;
  }

  /** Structural equality through JSON (the values here are plain data). */
  function sameValue(a: unknown, b: unknown): boolean {
    return JSON.stringify(a) === JSON.stringify(b);
  }

  /** The value shared by every item, or `mixed` when they differ. */
  function shared<T>(values: readonly T[]): { mixed: boolean; value: T | undefined } {
    const first = values[0];
    return { mixed: !values.every((v) => sameValue(v, first)), value: first };
  }

  /** Converts an `Rgba` to `#rrggbb` (black for null). */
  function rgbaToHex(c: Rgba | null | undefined): string {
    if (!c) return DEFAULT_DARK_HEX;
    const hex = (n: number): string =>
      clamp(Math.round(n), 0, 255).toString(16).padStart(2, '0');
    return `#${hex(c.r)}${hex(c.g)}${hex(c.b)}`;
  }

  /** Converts `#rrggbb` plus an alpha (0..255) to `Rgba`. Null when the text is not a hex colour. */
  function hexToRgba(hex: string, alpha: number): Rgba | null {
    const m = /^#([0-9a-fA-F]{6})$/.exec(hex);
    const body = m?.[1];
    if (body === undefined) return null;
    const n = parseInt(body, 16);
    return {
      r: (n >> 16) & 255,
      g: (n >> 8) & 255,
      b: n & 255,
      a: clamp(Math.round(alpha), 0, 255),
    };
  }

  /** Timecode text for a shared tick value, or Mixed. */
  function readout(values: readonly Tick[]): string {
    const s = shared(values);
    if (s.mixed) return 'Mixed';
    return s.value === undefined ? '' : formatTimecode(s.value, rate);
  }

  /** Name of the media a clip uses. */
  function mediaLabel(c: Clip): string {
    if (c.media_id === null) return 'None';
    return mediaById.get(c.media_id)?.name ?? 'Missing media';
  }

  /** Length spanned by a set of clips (last end minus first start), or 0. */
  function spanOf(list: readonly Clip[]): Tick {
    let min = Number.POSITIVE_INFINITY;
    let max = Number.NEGATIVE_INFINITY;
    for (const c of list) {
      min = Math.min(min, c.start);
      max = Math.max(max, clipEnd(c));
    }
    return max > min ? max - min : 0;
  }

  /** Shared value of one transform field across the visual clips. */
  function tf<K extends keyof Transform>(key: K): { mixed: boolean; value: Transform[K] | undefined } {
    return shared(visualClips.map((c) => c.transform[key]));
  }

  /** Whether a new transition may start on `c` in project `p`: visual, has an adjacent clip before it on its track, long enough. */
  function canStartTransition(p: Project, c: Clip): boolean {
    if (c.kind === 'audio') return false;
    const hasPredecessor = p.clips.some(
      (o) => o.id !== c.id && o.track_id === c.track_id && Math.abs(o.start + o.dur - c.start) <= 1,
    );
    return hasPredecessor && c.dur >= secToTick(MIN_CLIP_FOR_TRANSITION_S);
  }

  /** Gain (dB, clamped) that moves a measured clip to the goal, or null when it cannot be measured. */
  function targetGain(r: LoudnessReport, goal: NormGoal): number | null {
    if (!Number.isFinite(r.peakDb) || r.peakDb <= SILENT_PEAK_DB) return null;
    if (goal.kind === 'peak') return clamp(PEAK_TARGET_DB - r.peakDb, GAIN_MIN, GAIN_MAX);
    if (r.lufs === null) return null;
    return clamp(goal.target - r.lufs, GAIN_MIN, GAIN_MAX);
  }

  /** Returns `t` with one crop side set to `percent`, clamped so the opposite side still fits. */
  function setCrop(t: Transform, side: keyof Crop, percent: number): Transform {
    const room = Math.min(CROP_MAX, CROP_SUM_MAX - t.crop[OPPOSITE_SIDE[side]]);
    const value = clamp(percent / 100, 0, Math.max(0, room));
    return { ...t, crop: { ...t.crop, [side]: round(value, 4) } };
  }

  /** Fade-in or fade-out patch for one audio clip, clamped against the latest state of the clip. */
  function fadePatch(c: Clip, which: 'fade_in' | 'fade_out', seconds: number): ClipPatch {
    const want = secToTick(clamp(seconds, 0, tickToSec(c.dur)));
    if (which === 'fade_in') {
      return { fade_in: Math.min(want, Math.max(0, c.dur - c.audio.fade_out)) };
    }
    return { fade_out: Math.min(want, Math.max(0, c.dur - c.audio.fade_in)) };
  }

  // ---------------------------------------------------------------------------------------------
  // Commit plumbing: drafts show the pending value; commits go through `dispatch`/`dispatchWith`.
  // Every pending commit captures its target ids when it is staged. Pending commits run at once
  // (flushed) on a selection change and on destroy, so no typed value is lost or redirected.
  // ---------------------------------------------------------------------------------------------

  /** Pending numeric values (in display units) while a control is being moved or typed. */
  let numDrafts = $state<Record<string, number>>({});
  /** Pending text values while a text control is being typed in. */
  let strDrafts = $state<Record<string, string>>({});

  /** A commit waiting for its idle timer. `run` commits it now. */
  interface PendingCommit {
    timer: ReturnType<typeof setTimeout>;
    run: () => void;
  }
  /** Pending commits, keyed by control. */
  const pending = new Map<string, PendingCommit>();

  /** Cancels the timer of a pending commit (the commit itself is not run). */
  function cancelTimer(key: string): void {
    const entry = pending.get(key);
    if (entry !== undefined) {
      clearTimeout(entry.timer);
      pending.delete(key);
    }
  }

  /** Runs every pending commit now, against the targets captured when each was staged. */
  function flushPending(): void {
    for (const entry of [...pending.values()]) {
      clearTimeout(entry.timer);
      entry.run();
    }
  }

  onDestroy(() => {
    flushPending();
  });

  /** Selection key of the previous run, so a real selection change flushes pending commits. */
  let lastSelection = '';
  $effect(() => {
    const key = ids.join('|');
    if (key === lastSelection) return;
    lastSelection = key;
    untrack(() => {
      flushPending();
    });
  });

  /**
   * Shows `value` at once and commits it to `targets` after `delay` ms (or now when `delay` is 0).
   * `targets` are the ids at the moment of the edit, not whatever is selected when the timer fires.
   * The draft is dropped when the commit settles, unless a newer draft replaced it meanwhile.
   */
  function stage<T>(
    store: Record<string, T>,
    key: string,
    value: T,
    targets: readonly string[],
    commit: (v: T, targets: readonly string[]) => unknown,
    delay: number,
  ): void {
    store[key] = value;
    cancelTimer(key);
    const run = (): void => {
      cancelTimer(key);
      const done = (): void => {
        if (store[key] === value) delete store[key];
      };
      Promise.resolve(commit(value, targets)).then(done, done);
    };
    if (delay <= NOW) run();
    else pending.set(key, { timer: setTimeout(run, delay), run });
  }

  /** Stages the numeric value of an input element. Empty or invalid text is ignored. */
  function numIn(
    key: string,
    el: HTMLInputElement,
    delay: number,
    targets: readonly string[],
    commit: (v: number, targets: readonly string[]) => unknown,
  ): void {
    const v = el.valueAsNumber;
    if (!Number.isFinite(v)) return;
    stage(numDrafts, key, v, targets, commit, delay);
  }

  /** Shown numeric value: the pending draft, else the shared value times `factor`. */
  function shownNum(key: string, values: readonly number[], factor = 1): number | null {
    const draft = numDrafts[key];
    if (draft !== undefined) return draft;
    const s = shared(values);
    if (s.mixed || s.value === undefined) return null;
    return round(s.value * factor, 4);
  }

  /** Sets one shared patch on the listed clips, as a single command. */
  function applyPatch(targets: readonly string[], patch: ClipPatch): Promise<void> {
    if (targets.length === 0) return Promise.resolve();
    return dispatch({ type: 'patch_clips', ids: [...targets], patch }).then(() => undefined);
  }

  /**
   * Patches each listed clip with the patch `build` derives from the clip's LATEST state in the
   * project (so relative edits such as rotate or flip never use stale values). One command is sent
   * when every clip gets the same patch; otherwise one command per clip, in order.
   */
  async function patchEach(
    targets: readonly string[],
    build: (c: Clip, p: Project) => ClipPatch | null,
  ): Promise<void> {
    if (targets.length === 0) return;
    const state = { handled: false };
    await dispatchWith((p) => {
      const groups = new Map<string, { patch: ClipPatch; ids: string[] }>();
      for (const id of targets) {
        const c = p.clips.find((o) => o.id === id);
        const patch = c ? build(c, p) : null;
        if (patch === null) continue;
        const key = JSON.stringify(patch);
        const group = groups.get(key);
        if (group) group.ids.push(id);
        else groups.set(key, { patch, ids: [id] });
      }
      if (groups.size > 1) return null;
      state.handled = true;
      const only = [...groups.values()][0];
      return only ? { type: 'patch_clips', ids: only.ids, patch: only.patch } : null;
    });
    if (state.handled) return;
    for (const id of targets) {
      await dispatchWith((p) => {
        const c = p.clips.find((o) => o.id === id);
        const patch = c ? build(c, p) : null;
        return patch ? { type: 'patch_clips', ids: [id], patch } : null;
      });
    }
  }

  /** Replaces the transform of each listed visual clip with `change(its latest transform)`. */
  function setTransform(targets: readonly string[], change: (t: Transform) => Transform): Promise<void> {
    return patchEach(targets, (c) => ({ transform: change(c.transform) }));
  }

  /** Changes the text of each listed title clip with `change(its latest text)`. */
  function textPatch(targets: readonly string[], change: (t: TextSpec) => TextSpec): Promise<void> {
    return patchEach(targets, (c) => (c.text ? { text: change(c.text) } : null));
  }

  /** Runs commands one after another. */
  async function runSeq(cmds: Command[]): Promise<void> {
    for (const cmd of cmds) await dispatch(cmd);
  }

  // ---------------------------------------------------------------------------------------------
  // Speed, reverse
  // ---------------------------------------------------------------------------------------------

  let ripple = $state(true);

  /** Sets constant speed on the listed clips (the backend also changes partners). */
  function setSpeed(targets: readonly string[], v: number): Promise<void> {
    const speed = round(clamp(v, SPEED_MIN, SPEED_MAX), 2);
    return runSeq(
      targets.map((id): Command => ({ type: 'set_speed', clip_id: id, speed, ripple })),
    );
  }

  /** Sets reverse playback on the listed clips. */
  function setReverse(targets: readonly string[], reverse: boolean): Promise<void> {
    return runSeq(
      targets.map((id): Command => ({ type: 'set_reverse', clip_id: id, reverse })),
    );
  }

  // ---------------------------------------------------------------------------------------------
  // Transitions
  // ---------------------------------------------------------------------------------------------

  /** Transition kind of a clip, or 'none'. */
  function transitionChoice(c: Clip): TransitionChoice {
    return c.transition_in ? c.transition_in.kind : 'none';
  }

  /**
   * Sets the transition into each listed visual clip. 'none' clears every transition. A kind is
   * applied to clips that already have one, and started on clips where a new one is allowed.
   */
  function setTransition(targets: readonly string[], choice: TransitionChoice): Promise<void> {
    if (choice === 'none') {
      return applyPatch(targets, { transition_in: null });
    }
    const kind: TransitionKind = choice;
    return patchEach(targets, (c, p) => {
      if (c.transition_in) {
        return { transition_in: { kind, duration: c.transition_in.duration } };
      }
      if (!canStartTransition(p, c)) return null;
      return {
        transition_in: { kind, duration: Math.min(secToTick(DEFAULT_TRANSITION_S), c.dur) },
      };
    });
  }

  /** Sets the transition length in seconds on the listed clips that have a transition. */
  function setTransitionSeconds(targets: readonly string[], seconds: number): Promise<void> {
    const s = clamp(seconds, MIN_TRANSITION_S, MAX_TRANSITION_S);
    return patchEach(targets, (c) => {
      const tr = c.transition_in;
      if (!tr) return null;
      const duration = Math.min(secToTick(s), c.dur);
      if (duration < secToTick(MIN_TRANSITION_S)) return null;
      return { transition_in: { kind: tr.kind, duration } };
    });
  }

  // ---------------------------------------------------------------------------------------------
  // Audio: normalise, envelope
  // ---------------------------------------------------------------------------------------------

  /** One measured audio clip, shown under the normalise buttons. */
  interface MeasuredRow {
    id: string;
    name: string;
    report: LoudnessReport;
    /** Absolute gain set on the clip, or null when the clip was skipped as silent. */
    gainDb: number | null;
  }

  let measured = $state<MeasuredRow[]>([]);
  let normalising = $state(false);

  /**
   * Measures each audio clip (source only, without its clip gain) and sets its gain to the absolute
   * value that reaches the goal. Setting an absolute gain keeps repeated clicks from accumulating.
   */
  async function normalise(goal: NormGoal): Promise<void> {
    normalising = true;
    try {
      const list = audioClips;
      const rows: MeasuredRow[] = [];
      for (const c of list) {
        const report = await editorApi.measureClipLoudness(c.id);
        rows.push({ id: c.id, name: c.name, report, gainDb: targetGain(report, goal) });
      }
      measured = rows;
      for (const row of rows) {
        if (row.gainDb === null) continue;
        await applyPatch([row.id], { gain_db: round(row.gainDb, 2) });
      }
    } catch (e) {
      toast.set(String(e));
    } finally {
      normalising = false;
    }
  }

  /** Removes the volume envelope of the listed audio clips. */
  function clearEnvelope(targets: readonly string[]): Promise<void> {
    return runSeq(
      targets.map((id): Command => ({ type: 'set_envelope', clip_id: id, points: [] })),
    );
  }

  // ---------------------------------------------------------------------------------------------
  // Actions row
  // ---------------------------------------------------------------------------------------------

  function splitAtPlayhead(): void {
    void dispatch({ type: 'split_at', time: get(playhead), ids });
  }

  function duplicate(): void {
    const offset = spanOf(clips);
    if (offset <= 0) return;
    void dispatch({ type: 'duplicate_clips', ids, offset });
  }

  function freezeAtPlayhead(): void {
    const at = get(playhead);
    void runSeq(
      freezeTargets.map((c): Command => ({
        type: 'freeze_frame',
        clip_id: c.id,
        at,
        dur: secToTick(FREEZE_S),
      })),
    );
  }

  function detachAudio(): void {
    void runSeq(videoClips.map((c): Command => ({ type: 'detach_audio', clip_id: c.id })));
  }

  function toggleLink(): void {
    void dispatch(allLinked ? { type: 'unlink_clips', ids } : { type: 'link_clips', ids });
  }
</script>

{#if proj && clips.length > 0}
  <div class="insp">
    {#if anyLocked}
      <p class="locked" role="status">Track is locked</p>
    {/if}
    <fieldset class="props" disabled={anyLocked}>
      <section aria-label="General">
        <h3>General</h3>
        <label class="row">
          <span>Name</span>
          <input
            type="text"
            value={names.mixed ? '' : (names.value ?? '')}
            placeholder={names.mixed ? 'Mixed' : ''}
            onchange={(e) => {
              const v = e.currentTarget.value.trim();
              if (v.length > 0) void applyPatch(ids, { name: v });
            }}
          />
        </label>
        <div class="row">
          <span>Label colour</span>
          <div class="swatches" role="group" aria-label="Label colour">
            <button
              type="button"
              class="swatch none"
              aria-label="No label colour"
              aria-pressed={!colours.mixed && colours.value === null}
              onclick={() => void applyPatch(ids, { color: null })}
            >
              /
            </button>
            {#each LABEL_COLOURS as col (col)}
              <button
                type="button"
                class="swatch"
                style:background={col}
                aria-label={`Label colour ${col}`}
                aria-pressed={!colours.mixed && colours.value === col}
                onclick={() => void applyPatch(ids, { color: col })}
              ></button>
            {/each}
          </div>
        </div>
        <label class="row">
          <span>Enabled</span>
          <span class="pair">
            <input
              type="checkbox"
              checked={enabledState.value === true && !enabledState.mixed}
              onchange={(e) => void applyPatch(ids, { enabled: e.currentTarget.checked })}
            />
            {#if enabledState.mixed}<span class="mixed">Mixed</span>{/if}
          </span>
        </label>
        <div class="row"><span>Start</span><output>{readout(clips.map((c) => c.start))}</output></div>
        <div class="row"><span>Duration</span><output>{readout(clips.map((c) => c.dur))}</output></div>
        <div class="row">
          <span>Source in</span><output>{readout(clips.map((c) => c.src_in))}</output>
        </div>
        <div class="row">
          <span>Source out</span>
          <output>{readout(clips.map((c) => c.src_in + srcLen(c)))}</output>
        </div>
        <div class="row">
          <span>Media</span>
          <output>{mediaText}</output>
        </div>
      </section>

      {#if speedIds.length > 0}
        <section aria-label="Speed">
          <h3>Speed</h3>
          <div class="row">
            <span>Speed</span>
            <div class="pair">
              <input
                type="range"
                min={SPEED_MIN}
                max={SPEED_MAX}
                step="0.01"
                aria-label="Speed slider"
                value={shownNum('speed', speedClips.map((c) => c.speed)) ?? 1}
                oninput={(e) =>
                  numIn('speed', e.currentTarget, IDLE_MS, speedIds, (v, targets) =>
                    setSpeed(targets, v),
                  )}
                onchange={(e) =>
                  numIn('speed', e.currentTarget, NOW, speedIds, (v, targets) =>
                    setSpeed(targets, v),
                  )}
              />
              <input
                type="number"
                min={SPEED_MIN}
                max={SPEED_MAX}
                step="0.01"
                aria-label="Speed multiplier"
                placeholder={speedState.mixed ? 'Mixed' : ''}
                value={shownNum('speed', speedClips.map((c) => c.speed)) ?? ''}
                onchange={(e) =>
                  numIn('speed', e.currentTarget, NOW, speedIds, (v, targets) =>
                    setSpeed(targets, v),
                  )}
              />
              <span>x</span>
            </div>
          </div>
          <div class="btns" role="group" aria-label="Speed presets">
            {#each SPEED_PRESETS as p (p)}
              <button
                type="button"
                aria-pressed={!speedState.mixed && speedState.value === p}
                onclick={() => void setSpeed(speedIds, p)}>{p}x</button
              >
            {/each}
          </div>
          <label class="row">
            <span>Ripple</span>
            <input type="checkbox" bind:checked={ripple} />
          </label>
          <label class="row">
            <span>Reverse</span>
            <span class="pair">
              <input
                type="checkbox"
                checked={reverseState.value === true && !reverseState.mixed}
                onchange={(e) => void setReverse(speedIds, e.currentTarget.checked)}
              />
              {#if reverseState.mixed}<span class="mixed">Mixed</span>{/if}
            </span>
          </label>
          <p class="muted">Audio pitch follows speed.</p>
        </section>
      {/if}

      {#if visualIds.length > 0}
        <section aria-label="Framing">
          <h3>Framing</h3>
          <div class="row">
            <span>Fit</span>
            <select
              aria-label="Fit mode"
              value={tf('fit').value}
              onchange={(e) => {
                const f = FIT_MODES.find((m) => m === e.currentTarget.value);
                if (f) void setTransform(visualIds, (t) => ({ ...t, fit: f }));
              }}
            >
              {#each FIT_MODES as m (m)}
                <option value={m}>{FIT_LABELS[m]}</option>
              {/each}
            </select>
          </div>
          <div class="row">
            <span>Scale</span>
            <div class="pair">
              <input
                type="range"
                min="5"
                max="800"
                step="1"
                aria-label="Scale slider"
                value={shownNum('scale', visualClips.map((c) => c.transform.scale), 100) ?? 100}
                oninput={(e) =>
                  numIn('scale', e.currentTarget, IDLE_MS, visualIds, (v, targets) =>
                    setTransform(targets, (t) => ({ ...t, scale: clamp(v, 5, 800) / 100 })),
                  )}
                onchange={(e) =>
                  numIn('scale', e.currentTarget, NOW, visualIds, (v, targets) =>
                    setTransform(targets, (t) => ({ ...t, scale: clamp(v, 5, 800) / 100 })),
                  )}
              />
              <input
                type="number"
                min="5"
                max="800"
                step="1"
                aria-label="Scale percent"
                placeholder={tf('scale').mixed ? 'Mixed' : ''}
                value={shownNum('scale', visualClips.map((c) => c.transform.scale), 100) ?? ''}
                onchange={(e) =>
                  numIn('scale', e.currentTarget, NOW, visualIds, (v, targets) =>
                    setTransform(targets, (t) => ({ ...t, scale: clamp(v, 5, 800) / 100 })),
                  )}
              />
              <span>%</span>
            </div>
          </div>
          <div class="row">
            <span>Position X</span>
            <div class="pair">
              <input
                type="range"
                min="-100"
                max="100"
                step="1"
                aria-label="Position X slider"
                value={shownNum('posx', visualClips.map((c) => c.transform.pos_x), 100) ?? 0}
                oninput={(e) =>
                  numIn('posx', e.currentTarget, IDLE_MS, visualIds, (v, targets) =>
                    setTransform(targets, (t) => ({ ...t, pos_x: clamp(v, -100, 100) / 100 })),
                  )}
                onchange={(e) =>
                  numIn('posx', e.currentTarget, NOW, visualIds, (v, targets) =>
                    setTransform(targets, (t) => ({ ...t, pos_x: clamp(v, -100, 100) / 100 })),
                  )}
              />
              <input
                type="number"
                min="-100"
                max="100"
                step="1"
                aria-label="Position X percent"
                placeholder={tf('pos_x').mixed ? 'Mixed' : ''}
                value={shownNum('posx', visualClips.map((c) => c.transform.pos_x), 100) ?? ''}
                onchange={(e) =>
                  numIn('posx', e.currentTarget, NOW, visualIds, (v, targets) =>
                    setTransform(targets, (t) => ({ ...t, pos_x: clamp(v, -100, 100) / 100 })),
                  )}
              />
              <span>%</span>
            </div>
          </div>
          <div class="row">
            <span>Position Y</span>
            <div class="pair">
              <input
                type="range"
                min="-100"
                max="100"
                step="1"
                aria-label="Position Y slider"
                value={shownNum('posy', visualClips.map((c) => c.transform.pos_y), 100) ?? 0}
                oninput={(e) =>
                  numIn('posy', e.currentTarget, IDLE_MS, visualIds, (v, targets) =>
                    setTransform(targets, (t) => ({ ...t, pos_y: clamp(v, -100, 100) / 100 })),
                  )}
                onchange={(e) =>
                  numIn('posy', e.currentTarget, NOW, visualIds, (v, targets) =>
                    setTransform(targets, (t) => ({ ...t, pos_y: clamp(v, -100, 100) / 100 })),
                  )}
              />
              <input
                type="number"
                min="-100"
                max="100"
                step="1"
                aria-label="Position Y percent"
                placeholder={tf('pos_y').mixed ? 'Mixed' : ''}
                value={shownNum('posy', visualClips.map((c) => c.transform.pos_y), 100) ?? ''}
                onchange={(e) =>
                  numIn('posy', e.currentTarget, NOW, visualIds, (v, targets) =>
                    setTransform(targets, (t) => ({ ...t, pos_y: clamp(v, -100, 100) / 100 })),
                  )}
              />
              <span>%</span>
            </div>
          </div>
          <div class="row">
            <span>Rotation</span>
            <div class="pair">
              <button
                type="button"
                aria-label="Rotate 90 degrees left"
                onclick={() =>
                  void setTransform(visualIds, (t) => ({ ...t, rotate_90: (t.rotate_90 + 3) % 4 }))}
                >Left</button
              >
              <button
                type="button"
                aria-label="Rotate 90 degrees right"
                onclick={() =>
                  void setTransform(visualIds, (t) => ({ ...t, rotate_90: (t.rotate_90 + 1) % 4 }))}
                >Right</button
              >
              <span class="muted">
                {tf('rotate_90').mixed
                  ? 'Mixed'
                  : `${((tf('rotate_90').value ?? 0) * 90).toString()} deg`}
              </span>
            </div>
          </div>
          <div class="row">
            <span>Flip</span>
            <div class="btns">
              <button
                type="button"
                aria-pressed={tf('flip_h').value === true && !tf('flip_h').mixed}
                onclick={() => void setTransform(visualIds, (t) => ({ ...t, flip_h: !t.flip_h }))}
                >Horizontal</button
              >
              <button
                type="button"
                aria-pressed={tf('flip_v').value === true && !tf('flip_v').mixed}
                onclick={() => void setTransform(visualIds, (t) => ({ ...t, flip_v: !t.flip_v }))}
                >Vertical</button
              >
            </div>
          </div>
          <div class="row">
            <span>Crop %</span>
            <div class="crop">
              {#each CROP_SIDES as { side, label } (side)}
                <label class="crop-cell">
                  <span>{label}</span>
                  <input
                    type="number"
                    min="0"
                    max="90"
                    step="1"
                    placeholder={shared(visualClips.map((c) => c.transform.crop[side])).mixed
                      ? 'Mixed'
                      : ''}
                    value={shownNum(
                      `crop_${side}`,
                      visualClips.map((c) => c.transform.crop[side]),
                      100,
                    ) ?? ''}
                    onchange={(e) =>
                      numIn(`crop_${side}`, e.currentTarget, NOW, visualIds, (v, targets) =>
                        setTransform(targets, (t) => setCrop(t, side, v)),
                      )}
                  />
                </label>
              {/each}
            </div>
          </div>
          <div class="row">
            <span>Opacity</span>
            <div class="pair">
              <input
                type="range"
                min="0"
                max="100"
                step="1"
                aria-label="Opacity slider"
                value={shownNum('opacity', visualClips.map((c) => c.opacity), 100) ?? 100}
                oninput={(e) =>
                  numIn('opacity', e.currentTarget, IDLE_MS, visualIds, (v, targets) =>
                    applyPatch(targets, { opacity: clamp(v, 0, 100) / 100 }),
                  )}
                onchange={(e) =>
                  numIn('opacity', e.currentTarget, NOW, visualIds, (v, targets) =>
                    applyPatch(targets, { opacity: clamp(v, 0, 100) / 100 }),
                  )}
              />
              <input
                type="number"
                min="0"
                max="100"
                step="1"
                aria-label="Opacity percent"
                placeholder={shared(visualClips.map((c) => c.opacity)).mixed ? 'Mixed' : ''}
                value={shownNum('opacity', visualClips.map((c) => c.opacity), 100) ?? ''}
                onchange={(e) =>
                  numIn('opacity', e.currentTarget, NOW, visualIds, (v, targets) =>
                    applyPatch(targets, { opacity: clamp(v, 0, 100) / 100 }),
                  )}
              />
              <span>%</span>
            </div>
          </div>
          <div class="btns">
            <button
              type="button"
              onclick={() => void applyPatch(visualIds, { transform: defaultTransform() })}
              >Reset transform</button
            >
          </div>
        </section>

        <section aria-label="Transition in">
          <h3>Transition in</h3>
          <div class="row">
            <span>Type</span>
            <select
              aria-label="Transition type"
              value={transitionState.mixed ? '' : (transitionState.value ?? 'none')}
              onchange={(e) => {
                const choice = TRANSITION_CHOICES.find((t) => t.value === e.currentTarget.value);
                if (choice) void setTransition(visualIds, choice.value);
              }}
            >
              {#each TRANSITION_CHOICES as t (t.value)}
                <option value={t.value} disabled={t.value !== 'none' && !transitionChangeable}
                  >{t.label}</option
                >
              {/each}
            </select>
          </div>
          <div class="row">
            <span>Length (s)</span>
            <div class="pair">
              <input
                type="range"
                min={MIN_TRANSITION_S}
                max={MAX_TRANSITION_S}
                step="0.1"
                aria-label="Transition length slider"
                disabled={!anyTransition}
                value={shownNum(
                  'tdur',
                  visualClips.flatMap((c) => (c.transition_in ? [tickToSec(c.transition_in.duration)] : [])),
                ) ?? DEFAULT_TRANSITION_S}
                oninput={(e) =>
                  numIn('tdur', e.currentTarget, IDLE_MS, visualIds, (v, targets) =>
                    setTransitionSeconds(targets, v),
                  )}
                onchange={(e) =>
                  numIn('tdur', e.currentTarget, NOW, visualIds, (v, targets) =>
                    setTransitionSeconds(targets, v),
                  )}
              />
              <input
                type="number"
                min={MIN_TRANSITION_S}
                max={MAX_TRANSITION_S}
                step="0.1"
                aria-label="Transition length seconds"
                disabled={!anyTransition}
                value={shownNum(
                  'tdur',
                  visualClips.flatMap((c) => (c.transition_in ? [tickToSec(c.transition_in.duration)] : [])),
                ) ?? DEFAULT_TRANSITION_S}
                onchange={(e) =>
                  numIn('tdur', e.currentTarget, NOW, visualIds, (v, targets) =>
                    setTransitionSeconds(targets, v),
                  )}
              />
            </div>
          </div>
          {#if transitionNote}
            <p class="muted">
              Needs an adjacent clip before it on the track, and clips shorter than
              {MIN_CLIP_FOR_TRANSITION_S} s.
            </p>
          {/if}
        </section>
      {/if}

      {#if titleIds.length > 0}
        <section aria-label="Title text">
          <h3>Title</h3>
          <label class="stack">
            <span>Text</span>
            <textarea
              placeholder={titleText.mixed ? 'Mixed' : ''}
              value={strDrafts['text'] ?? (titleText.mixed ? '' : (firstTitle?.text ?? ''))}
              oninput={(e) =>
                stage(
                  strDrafts,
                  'text',
                  e.currentTarget.value,
                  titleIds,
                  (t, targets) => textPatch(targets, (s) => ({ ...s, text: t })),
                  TEXT_IDLE_MS,
                )}
            ></textarea>
          </label>
          <div class="row">
            <span>Font</span>
            <input
              type="text"
              list="editor-title-fonts"
              aria-label="Font family"
              value={firstTitle?.font ?? ''}
              onchange={(e) => {
                const v = e.currentTarget.value.trim();
                if (v.length > 0) void textPatch(titleIds, (s) => ({ ...s, font: v }));
              }}
            />
            <datalist id="editor-title-fonts">
              {#each FONTS as f (f)}
                <option value={f}></option>
              {/each}
            </datalist>
          </div>
          <div class="row">
            <span>Size</span>
            <div class="pair">
              <input
                type="range"
                min={TEXT_SIZE_MIN}
                max={TEXT_SIZE_MAX}
                step="1"
                aria-label="Font size slider"
                value={shownNum('tsize', titleClips.flatMap((c) => (c.text ? [c.text.size] : []))) ?? 48}
                oninput={(e) =>
                  numIn('tsize', e.currentTarget, IDLE_MS, titleIds, (v, targets) =>
                    textPatch(targets, (s) => ({ ...s, size: clamp(v, TEXT_SIZE_MIN, TEXT_SIZE_MAX) })),
                  )}
                onchange={(e) =>
                  numIn('tsize', e.currentTarget, NOW, titleIds, (v, targets) =>
                    textPatch(targets, (s) => ({ ...s, size: clamp(v, TEXT_SIZE_MIN, TEXT_SIZE_MAX) })),
                  )}
              />
              <input
                type="number"
                min={TEXT_SIZE_MIN}
                max={TEXT_SIZE_MAX}
                step="1"
                aria-label="Font size"
                value={shownNum('tsize', titleClips.flatMap((c) => (c.text ? [c.text.size] : []))) ?? ''}
                onchange={(e) =>
                  numIn('tsize', e.currentTarget, NOW, titleIds, (v, targets) =>
                    textPatch(targets, (s) => ({ ...s, size: clamp(v, TEXT_SIZE_MIN, TEXT_SIZE_MAX) })),
                  )}
              />
            </div>
          </div>
          <div class="btns">
            <button
              type="button"
              aria-pressed={firstTitle?.bold === true}
              onclick={() => void textPatch(titleIds, (s) => ({ ...s, bold: !s.bold }))}>Bold</button
            >
            <button
              type="button"
              aria-pressed={firstTitle?.italic === true}
              onclick={() => void textPatch(titleIds, (s) => ({ ...s, italic: !s.italic }))}
              >Italic</button
            >
          </div>
          <div class="row">
            <span>Text colour</span>
            <input
              type="color"
              aria-label="Text colour"
              value={rgbaToHex(firstTitle?.color)}
              onchange={(e) => {
                const hex = e.currentTarget.value;
                void textPatch(titleIds, (s) => {
                  const rgba = hexToRgba(hex, s.color.a);
                  return rgba ? { ...s, color: rgba } : s;
                });
              }}
            />
          </div>
          <div class="row">
            <span>Alignment</span>
            <div class="btns" role="group" aria-label="Alignment">
              {#each ALIGNS as a (a.value)}
                <button
                  type="button"
                  aria-pressed={firstTitle?.align === a.value}
                  onclick={() => void textPatch(titleIds, (s) => ({ ...s, align: a.value }))}
                  >{a.label}</button
                >
              {/each}
            </div>
          </div>
          <div class="row">
            <span>Outline</span>
            <div class="pair">
              <input
                type="checkbox"
                aria-label="Outline on"
                checked={Boolean(firstTitle?.outline)}
                onchange={(e) =>
                  void textPatch(titleIds, (s) => ({
                    ...s,
                    outline: e.currentTarget.checked ? hexToRgba(DEFAULT_DARK_HEX, 255) : null,
                  }))}
              />
              <input
                type="color"
                aria-label="Outline colour"
                disabled={!firstTitle?.outline}
                value={rgbaToHex(firstTitle?.outline)}
                onchange={(e) => {
                  const hex = e.currentTarget.value;
                  void textPatch(titleIds, (s) => {
                    const rgba = hexToRgba(hex, s.outline?.a ?? 255);
                    return rgba ? { ...s, outline: rgba } : s;
                  });
                }}
              />
            </div>
          </div>
          <div class="row">
            <span>Background box</span>
            <div class="pair">
              <input
                type="checkbox"
                aria-label="Background box on"
                checked={Boolean(firstTitle?.box_bg)}
                onchange={(e) =>
                  void textPatch(titleIds, (s) => ({
                    ...s,
                    box_bg: e.currentTarget.checked ? hexToRgba(DEFAULT_DARK_HEX, BOX_ALPHA) : null,
                  }))}
              />
              <input
                type="color"
                aria-label="Background box colour"
                disabled={!firstTitle?.box_bg}
                value={rgbaToHex(firstTitle?.box_bg)}
                onchange={(e) => {
                  const hex = e.currentTarget.value;
                  void textPatch(titleIds, (s) => {
                    const rgba = hexToRgba(hex, s.box_bg?.a ?? BOX_ALPHA);
                    return rgba ? { ...s, box_bg: rgba } : s;
                  });
                }}
              />
            </div>
          </div>
        </section>
      {/if}

      {#if solidIds.length > 0}
        <section aria-label="Solid colour">
          <h3>Solid</h3>
          <div class="row">
            <span>Colour</span>
            <input
              type="color"
              aria-label="Solid colour"
              value={rgbaToHex(firstSolid)}
              onchange={(e) => {
                const hex = e.currentTarget.value;
                void patchEach(solidIds, (c) => {
                  const rgba = hexToRgba(hex, c.solid?.a ?? 255);
                  return rgba ? { solid: rgba } : null;
                });
              }}
            />
          </div>
        </section>
      {/if}

      {#if audioIds.length > 0}
        <section aria-label="Audio">
          <h3>Audio</h3>
          <div class="row">
            <span>Gain (dB)</span>
            <div class="pair">
              <input
                type="range"
                min={GAIN_MIN}
                max={GAIN_MAX}
                step="0.5"
                aria-label="Gain slider"
                value={shownNum('gain', audioClips.map((c) => c.audio.gain_db)) ?? 0}
                oninput={(e) =>
                  numIn('gain', e.currentTarget, IDLE_MS, audioIds, (v, targets) =>
                    applyPatch(targets, { gain_db: clamp(v, GAIN_MIN, GAIN_MAX) }),
                  )}
                onchange={(e) =>
                  numIn('gain', e.currentTarget, NOW, audioIds, (v, targets) =>
                    applyPatch(targets, { gain_db: clamp(v, GAIN_MIN, GAIN_MAX) }),
                  )}
              />
              <input
                type="number"
                min={GAIN_MIN}
                max={GAIN_MAX}
                step="0.5"
                aria-label="Gain dB"
                placeholder={shared(audioClips.map((c) => c.audio.gain_db)).mixed ? 'Mixed' : ''}
                value={shownNum('gain', audioClips.map((c) => c.audio.gain_db)) ?? ''}
                onchange={(e) =>
                  numIn('gain', e.currentTarget, NOW, audioIds, (v, targets) =>
                    applyPatch(targets, { gain_db: clamp(v, GAIN_MIN, GAIN_MAX) }),
                  )}
              />
            </div>
          </div>
          <label class="row">
            <span>Mute</span>
            <input
              type="checkbox"
              checked={shared(audioClips.map((c) => c.audio.muted)).value === true &&
                !shared(audioClips.map((c) => c.audio.muted)).mixed}
              onchange={(e) => void applyPatch(audioIds, { muted: e.currentTarget.checked })}
            />
          </label>
          <div class="row">
            <span>Pan</span>
            <div class="pair">
              <input
                type="range"
                min="-100"
                max="100"
                step="1"
                aria-label="Pan slider"
                value={shownNum('pan', audioClips.map((c) => c.audio.pan), 100) ?? 0}
                oninput={(e) =>
                  numIn('pan', e.currentTarget, IDLE_MS, audioIds, (v, targets) =>
                    applyPatch(targets, { pan: clamp(v, -100, 100) / 100 }),
                  )}
                onchange={(e) =>
                  numIn('pan', e.currentTarget, NOW, audioIds, (v, targets) =>
                    applyPatch(targets, { pan: clamp(v, -100, 100) / 100 }),
                  )}
              />
              <span class="muted">{panText}</span>
            </div>
          </div>
          <label class="row">
            <span>Mono downmix</span>
            <input
              type="checkbox"
              checked={shared(audioClips.map((c) => c.audio.mono)).value === true &&
                !shared(audioClips.map((c) => c.audio.mono)).mixed}
              onchange={(e) => void applyPatch(audioIds, { mono: e.currentTarget.checked })}
            />
          </label>
          <div class="row">
            <span>Source stream</span>
            <select
              aria-label="Source audio stream"
              value={shared(audioClips.map((c) => c.audio.source_track)).value}
              onchange={(e) => {
                const n = Number(e.currentTarget.value);
                if (Number.isInteger(n)) void applyPatch(audioIds, { source_track: n });
              }}
            >
              {#each streamChoices as s (s.value)}
                <option value={s.value}>{s.label}</option>
              {/each}
            </select>
          </div>
          <div class="row">
            <span>Fade in (s)</span>
            <input
              type="number"
              min="0"
              step="0.1"
              aria-label="Fade in seconds"
              placeholder={shared(audioClips.map((c) => c.audio.fade_in)).mixed ? 'Mixed' : ''}
              value={shownNum('fadein', audioClips.map((c) => tickToSec(c.audio.fade_in))) ?? ''}
              onchange={(e) =>
                numIn('fadein', e.currentTarget, NOW, audioIds, (v, targets) =>
                  patchEach(targets, (c) => fadePatch(c, 'fade_in', v)),
                )}
            />
          </div>
          <div class="row">
            <span>Fade out (s)</span>
            <input
              type="number"
              min="0"
              step="0.1"
              aria-label="Fade out seconds"
              placeholder={shared(audioClips.map((c) => c.audio.fade_out)).mixed ? 'Mixed' : ''}
              value={shownNum('fadeout', audioClips.map((c) => tickToSec(c.audio.fade_out))) ?? ''}
              onchange={(e) =>
                numIn('fadeout', e.currentTarget, NOW, audioIds, (v, targets) =>
                  patchEach(targets, (c) => fadePatch(c, 'fade_out', v)),
                )}
            />
          </div>
          <div class="row">
            <span>Fade curve</span>
            <select
              aria-label="Fade curve"
              value={shared(audioClips.map((c) => c.audio.fade_curve)).value}
              onchange={(e) => {
                const curve = FADE_CURVES.find((f) => f.value === e.currentTarget.value);
                if (curve) void applyPatch(audioIds, { fade_curve: curve.value });
              }}
            >
              {#each FADE_CURVES as f (f.value)}
                <option value={f.value}>{f.label}</option>
              {/each}
            </select>
          </div>
          <div class="btns" role="group" aria-label="Normalise">
            <button
              type="button"
              disabled={normalising}
              onclick={() => void normalise({ kind: 'peak' })}>Normalise peak -1 dBFS</button
            >
            <button
              type="button"
              disabled={normalising}
              onclick={() => void normalise({ kind: 'lufs', target: -14 })}>-14 LUFS</button
            >
            <button
              type="button"
              disabled={normalising}
              onclick={() => void normalise({ kind: 'lufs', target: -16 })}>-16 LUFS</button
            >
          </div>
          {#if measured.length > 0}
            <ul class="measured">
              {#each measured as m (m.id)}
                <li>
                  {m.name}: peak {m.report.peakDb.toFixed(1)} dBFS,
                  {m.report.lufs === null ? 'silent' : `${m.report.lufs.toFixed(1)} LUFS`},
                  {m.gainDb === null
                    ? 'clip is silent, skipped'
                    : `gain set to ${m.gainDb.toFixed(1)} dB`}
                </li>
              {/each}
            </ul>
          {/if}
          <div class="btns">
            <button
              type="button"
              disabled={envelopeIds.length === 0}
              onclick={() => void clearEnvelope(envelopeIds)}>Clear envelope</button
            >
          </div>
          <p class="muted">Envelope points are edited on the timeline (clip gain line).</p>
        </section>
      {/if}
    </fieldset>

    <section aria-label="Actions">
      <h3>Actions</h3>
      <div class="btns">
        <button type="button" onclick={splitAtPlayhead}>Split at playhead</button>
        <button type="button" onclick={duplicate}>Duplicate</button>
        {#if videoClips.length > 0}
          <button type="button" disabled={freezeTargets.length === 0} onclick={freezeAtPlayhead}
            >Freeze frame at playhead</button
          >
          <button type="button" onclick={detachAudio}>Detach audio</button>
        {/if}
        <button type="button" onclick={() => void dispatch({ type: 'group_clips', ids })}>Group</button>
        <button type="button" onclick={() => void dispatch({ type: 'ungroup', ids })}>Ungroup</button>
        <button type="button" onclick={toggleLink}>{allLinked ? 'Unlink' : 'Link'}</button>
      </div>
    </section>
  </div>
{/if}

<style>
  .insp {
    display: flex;
    flex-direction: column;
    color: var(--text);
    font-size: 13px;
  }
  fieldset.props {
    display: flex;
    flex-direction: column;
    min-width: 0;
    margin: 0;
    padding: 0;
    border: 0;
  }
  section {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 10px 12px;
    border-top: 1px solid var(--border);
  }
  h3 {
    margin: 0;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-dim);
  }
  .locked {
    margin: 0;
    padding: 8px 12px 0;
    color: var(--accent);
    font-size: 12px;
  }
  .row {
    display: grid;
    grid-template-columns: 110px minmax(0, 1fr);
    align-items: center;
    gap: 8px;
  }
  .row > span:first-child,
  .stack > span {
    color: var(--text-dim);
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .pair {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .pair input[type='range'] {
    flex: 1;
    min-width: 0;
  }
  .btns {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .crop {
    display: grid;
    grid-template-columns: repeat(4, minmax(0, 1fr));
    gap: 6px;
  }
  .crop-cell {
    display: flex;
    flex-direction: column;
    gap: 2px;
    color: var(--text-dim);
    font-size: 11px;
  }
  .swatches {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
  }
  .swatch {
    width: 20px;
    height: 20px;
    padding: 0;
    border-radius: 50%;
    border: 2px solid var(--surface);
    cursor: pointer;
    font-size: 11px;
    color: var(--text-dim);
  }
  .swatch[aria-pressed='true'] {
    outline: 2px solid var(--text);
    outline-offset: 1px;
  }
  input[type='number'],
  input[type='text'],
  select,
  textarea {
    min-width: 0;
    padding: 4px 6px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface-2);
    color: var(--text);
  }
  input[type='number'] {
    width: 76px;
  }
  textarea {
    width: 100%;
    min-height: 64px;
    resize: vertical;
  }
  input[type='color'] {
    width: 36px;
    height: 26px;
    padding: 0;
    border: 1px solid var(--border);
    background: none;
    cursor: pointer;
  }
  button {
    padding: 4px 8px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface-2);
    color: var(--text);
    cursor: pointer;
  }
  button[aria-pressed='true'] {
    border-color: var(--accent);
    color: var(--accent);
  }
  button:disabled,
  input:disabled,
  select:disabled,
  textarea:disabled {
    opacity: 0.45;
    cursor: default;
  }
  button:focus-visible,
  input:focus-visible,
  select:focus-visible,
  textarea:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .muted {
    margin: 0;
    color: var(--text-dim);
    font-size: 12px;
  }
  .mixed {
    color: var(--text-dim);
    font-style: italic;
  }
  .measured {
    margin: 0;
    padding-left: 18px;
    color: var(--text-dim);
    font-size: 12px;
  }
</style>
