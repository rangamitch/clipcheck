<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { get } from 'svelte/store';
  import { editorApi } from './api';
  import { createPreviewEngine, type PreviewEngine } from './preview/engine';
  import { hitLayerAt, projectToCanvasRect } from './preview/compose';
  import { loopInOut, runAction, shuttleRate, type ActionContext } from './actions';
  import { ACTIONS } from './shortcuts';
  import {
    dispatch,
    playhead,
    playing,
    project,
    seek,
    selectClips,
    selectedClips,
    selection,
    snapshot,
    timelineEndTick,
    tool,
    toast,
  } from './store';
  import {
    clipEnd,
    formatDuration,
    formatTimecode,
    frameDurationTicks,
    snapToFrame,
  } from './ticks';
  import {
    defaultTransform,
    type Clip,
    type Project,
    type Rate,
    type Tick,
    type Transform,
  } from './types';
  import Icon from '../player/Icon.svelte';

  /** Preview quality choices. `auto` starts at full quality (see `effectiveQuality`). */
  type Quality = 'auto' | 'full' | 'half' | 'quarter';
  /** Transform handle positions as fractions of the picture box (x, y). */
  type Handle = 'nw' | 'n' | 'ne' | 'e' | 'se' | 's' | 'sw' | 'w';
  /** A rectangle in canvas pixels. */
  interface CanvasRect {
    x: number;
    y: number;
    w: number;
    h: number;
  }
  /** Point in canvas pixels. */
  interface Point {
    x: number;
    y: number;
  }
  /** Transform drag in progress. `base` is the stored transform, `orig` the same look in custom mode. */
  interface Drag {
    mode: 'move' | Handle;
    clipId: string;
    start: Point;
    center: Point;
    base: Transform;
    orig: Transform;
  }
  /** Preview quality the auto mode can settle on (it only ever steps down). */
  type AutoLevel = 'full' | 'half' | 'quarter';

  const VOLUME_KEY = 'clipcheck.editor.program.volume';
  const MUTED_KEY = 'clipcheck.editor.program.muted';
  /** Pointer distance (screen px) that still counts as hitting a handle. */
  const HANDLE_HIT_PX = 9;
  /** Scale and position limits from the timeline model. */
  const SCALE_MIN = 0.05;
  const SCALE_MAX = 8;
  const POS_LIMIT = 2;
  /** Arrow key nudge as a fraction of the canvas, and with Shift. */
  const NUDGE = 0.01;
  const NUDGE_SHIFT = 0.1;
  /** Meter floor in dBFS. */
  const METER_FLOOR_DB = -60;
  const HANDLES: readonly Handle[] = ['nw', 'n', 'ne', 'e', 'se', 's', 'sw', 'w'];
  const HANDLE_FRACTION: Record<Handle, Point> = {
    nw: { x: 0, y: 0 },
    n: { x: 0.5, y: 0 },
    ne: { x: 1, y: 0 },
    e: { x: 1, y: 0.5 },
    se: { x: 1, y: 1 },
    s: { x: 0.5, y: 1 },
    sw: { x: 0, y: 1 },
    w: { x: 0, y: 0.5 },
  };
  const VISUAL_KINDS = new Set(['video', 'image', 'title', 'solid']);
  /** Share of dropped frames above which auto quality steps down, and reports in a row needed. */
  const AUTO_DROP_RATIO = 0.1;
  const AUTO_DROP_STREAK = 2;
  /** Context for `play_pause`, which does not read the view fields. */
  const PLAY_CONTEXT: ActionContext = {
    openExport: () => undefined,
    showShortcuts: () => undefined,
    viewWidth: 0,
    hasSource: false,
  };

  /** Clamps to 0..1, with non-finite values giving 1. */
  function clamp01(v: number): number {
    return Number.isFinite(v) ? Math.min(1, Math.max(0, v)) : 1;
  }

  /** Volume and mute remembered across sessions. Storage failures fall back to defaults. */
  function loadAudioPrefs(): { volume: number; muted: boolean } {
    try {
      const raw = localStorage.getItem(VOLUME_KEY);
      return {
        volume: raw === null ? 1 : clamp01(Number(raw)),
        muted: localStorage.getItem(MUTED_KEY) === '1',
      };
    } catch {
      return { volume: 1, muted: false };
    }
  }

  /** Saves volume and mute. Storage failures are ignored (the setting still works this session). */
  function saveAudioPrefs(volume: number, muted: boolean): void {
    try {
      localStorage.setItem(VOLUME_KEY, String(volume));
      localStorage.setItem(MUTED_KEY, muted ? '1' : '0');
    } catch {
      // Storage unavailable: nothing to persist.
    }
  }

  /** dBFS to a 0..100 bar length. */
  function meterPercent(db: number): number {
    if (!Number.isFinite(db)) return 0;
    return Math.min(100, Math.max(0, ((db - METER_FLOOR_DB) / -METER_FLOOR_DB) * 100));
  }

  const initialAudio = loadAudioPrefs();

  let stageEl = $state<HTMLDivElement | undefined>(undefined);
  let frameEl = $state<HTMLDivElement | undefined>(undefined);
  let canvasEl = $state<HTMLCanvasElement | undefined>(undefined);
  /** Engine created on mount. Raw so the class instance is not proxied. */
  let engine = $state.raw<PreviewEngine | null>(null);

  let volume = $state(initialAudio.volume);
  let muted = $state(initialAudio.muted);
  let quality = $state<Quality>('auto');
  /** Level auto quality currently uses. Only steps down within the session. */
  let autoLevel = $state<AutoLevel>('full');
  let showGuides = $state(false);
  let isFullscreen = $state(false);
  /** Master level in dBFS. */
  let meterDb = $state(METER_FLOOR_DB);
  /** Unavailable-media and decode messages from the engine, oldest first. */
  let mediaErrors = $state<string[]>([]);
  /** Transform being dragged, shown live before it is committed. */
  let draft = $state<{ clipId: string; transform: Transform } | null>(null);

  /** Last tick the engine reported. Playhead updates that match it are echoes, not seeks. */
  let lastEngineTick: Tick = Number.NaN;
  /** Active transform drag (not reactive: only `draft` is shown). */
  let drag: Drag | null = null;
  /** Consecutive dropped-frame reports above the auto threshold. */
  let droppedStreak = 0;
  /** Clip whose transform the engine currently overrides during a drag, or null. */
  let overrideClipId: string | null = null;

  const canvas = $derived($snapshot?.project.canvas ?? null);
  const cw = $derived(canvas?.width ?? 1920);
  const ch = $derived(canvas?.height ?? 1080);
  const fps = $derived<Rate>(canvas?.fps ?? { num: 30, den: 1 });
  const frameTicks = $derived(frameDurationTicks(fps));
  const timelineEnd = $derived(timelineEndTick($project));
  const workArea = $derived($project?.work_area ?? null);
  const canLoop = $derived(workArea !== null && workArea.end > workArea.start);
  /** Clip that gets transform handles: one visual clip selected, Select tool, not playing. */
  const editClip = $derived.by(() => {
    if ($tool !== 'select' || $playing) return null;
    const [only] = $selectedClips;
    if ($selectedClips.length !== 1 || only === undefined) return null;
    if (!VISUAL_KINDS.has(only.kind)) return null;
    // Handles show only while the clip is the one under the playhead.
    return only.start <= $playhead && $playhead < clipEnd(only) ? only : null;
  });
  /** Transform shown for the edit clip: the live draft while dragging, else the stored one. */
  const shownTransform = $derived.by((): Transform | null => {
    if (editClip === null) return null;
    if (draft !== null && draft.clipId === editClip.id) return draft.transform;
    return editClip.transform;
  });
  /** Picture rectangle of the edit clip in canvas pixels. */
  const editRect = $derived.by((): CanvasRect | null => {
    if (editClip === null || shownTransform === null) return null;
    return clipRect(editClip.id, shownTransform);
  });
  const frameRatio = $derived(`${cw} / ${ch}`);
  const effectiveQuality = $derived<AutoLevel>(quality === 'auto' ? autoLevel : quality);

  /** Source pixel size of a media id; the project canvas size when there is no media (titles). */
  function sourceSize(project: Project, mediaId: string | null): { w: number; h: number } {
    const m = mediaId === null ? undefined : project.media.find((x) => x.id === mediaId);
    if (m === undefined) return { w: project.canvas.width, h: project.canvas.height };
    return { w: m.width, h: m.height };
  }

  /**
   * Rectangle of clip `clipId` drawn with `transform`, in project canvas pixels (the coordinates
   * of the picture, independent of the preview resolution). Null when the clip is unknown or the
   * rectangle is empty.
   */
  function clipRect(clipId: string, transform: Transform): CanvasRect | null {
    const s = get(snapshot);
    if (s === null) return null;
    const clip = s.project.clips.find((c) => c.id === clipId);
    if (clip === undefined) return null;
    const size = sourceSize(s.project, clip.media_id);
    const r = projectToCanvasRect(s.project, transform, size.w, size.h, cw, ch);
    return r.w > 0 && r.h > 0 ? r : null;
  }

  /** Maps a pointer event to canvas pixels, or null when the frame is not laid out. */
  function toCanvasPoint(e: { clientX: number; clientY: number }): Point | null {
    const r = frameEl?.getBoundingClientRect();
    if (r === undefined || r.width <= 0 || r.height <= 0) return null;
    return {
      x: ((e.clientX - r.left) / r.width) * cw,
      y: ((e.clientY - r.top) / r.height) * ch,
    };
  }

  /** Canvas pixels per screen pixel of the frame (for the handle hit radius). */
  function canvasPerScreenPx(): number {
    const r = frameEl?.getBoundingClientRect();
    if (r === undefined || r.width <= 0) return 1;
    return cw / r.width;
  }

  /** Handle under canvas point `p` for rectangle `r`, or null. */
  function handleAt(r: CanvasRect, p: Point): Handle | null {
    const tol = HANDLE_HIT_PX * canvasPerScreenPx();
    for (const h of HANDLES) {
      const f = HANDLE_FRACTION[h];
      const hx = r.x + f.x * r.w;
      const hy = r.y + f.y * r.h;
      if (Math.abs(p.x - hx) <= tol && Math.abs(p.y - hy) <= tol) return h;
    }
    return null;
  }

  function inside(r: CanvasRect, p: Point): boolean {
    return p.x >= r.x && p.x <= r.x + r.w && p.y >= r.y && p.y <= r.y + r.h;
  }

  /** Clip under canvas point `p` at the playhead (topmost layer), or null. */
  function hitClip(p: Point): string | null {
    const s = get(snapshot);
    if (s === null) return null;
    const t = get(playhead);
    const seg = s.plan.segments.find((sg) => t >= sg.start && t < sg.end);
    if (seg === undefined) return null;
    const sizes = (mediaId: string): { w: number; h: number } | null => {
      const m = s.project.media.find((x) => x.id === mediaId);
      return m === undefined ? null : { w: m.width, h: m.height };
    };
    return hitLayerAt(s.project, seg, p.x, p.y, cw, ch, sizes);
  }

  function sameTransform(a: Transform, b: Transform): boolean {
    return (
      a.fit === b.fit &&
      a.scale === b.scale &&
      a.pos_x === b.pos_x &&
      a.pos_y === b.pos_y
    );
  }

  function clampScale(v: number): number {
    return Math.min(SCALE_MAX, Math.max(SCALE_MIN, v));
  }

  function clampPos(v: number): number {
    return Math.min(POS_LIMIT, Math.max(-POS_LIMIT, v));
  }

  /** Scale factor of a handle drag, measured from the centre of the picture. */
  function scaleFactor(h: Handle, d: Drag, p: Point): number {
    const dx0 = Math.abs(d.start.x - d.center.x);
    const dy0 = Math.abs(d.start.y - d.center.y);
    const dx = Math.abs(p.x - d.center.x);
    const dy = Math.abs(p.y - d.center.y);
    if (h === 'e' || h === 'w') return dx0 > 0 ? dx / dx0 : 1;
    if (h === 'n' || h === 's') return dy0 > 0 ? dy / dy0 : 1;
    const d0 = Math.hypot(dx0, dy0);
    return d0 > 0 ? Math.hypot(dx, dy) / d0 : 1;
  }

  /** Pointer down: start a handle or move drag on the edit clip, else select the clip under the pointer. */
  function onPointerDown(e: PointerEvent): void {
    if (e.button !== 0) return;
    const p = toCanvasPoint(e);
    if (p === null) return;
    const clip = editClip;
    const rect = editRect;
    const transform = shownTransform;
    if (clip !== null && rect !== null && transform !== null) {
      const h = handleAt(rect, p);
      if (h !== null || inside(rect, p)) {
        drag = {
          mode: h ?? 'move',
          clipId: clip.id,
          start: p,
          center: { x: rect.x + rect.w / 2, y: rect.y + rect.h / 2 },
          base: transform,
          orig: toCustom(clip, transform),
        };
        frameEl?.setPointerCapture(e.pointerId);
        return;
      }
    }
    const hit = hitClip(p);
    if (hit !== null) selectClips([hit], 'replace');
    else selection.set([]);
  }

  /**
   * Transform in custom mode that draws the clip where `orig` draws it. Fit, Fill and Stretch
   * clips get a scale relative to the fit rectangle, so the first drag does not jump.
   */
  function toCustom(clip: Clip, orig: Transform): Transform {
    if (orig.fit === 'custom') return orig;
    const s = get(snapshot);
    if (s === null) return { ...orig, fit: 'custom' };
    const size = sourceSize(s.project, clip.media_id);
    const fitRect = projectToCanvasRect(
      s.project,
      { ...orig, fit: 'fit', scale: 1, pos_x: 0, pos_y: 0 },
      size.w,
      size.h,
      cw,
      ch,
    );
    const cur = projectToCanvasRect(s.project, orig, size.w, size.h, cw, ch);
    if (fitRect.w <= 0 || cur.w <= 0) return { ...orig, fit: 'custom' };
    return {
      ...orig,
      fit: 'custom',
      scale: clampScale(cur.w / fitRect.w),
      pos_x: 0,
      pos_y: 0,
    };
  }

  /** Shows `transform` for `clipId` in the monitor while a drag is in progress. */
  function setDraft(clipId: string, transform: Transform): void {
    draft = { clipId, transform };
    overrideClipId = clipId;
    engine?.setTransformOverride(clipId, transform);
  }

  /** Drops the live draft and the engine override. */
  function releaseDraft(): void {
    draft = null;
    if (overrideClipId !== null) engine?.setTransformOverride(overrideClipId, null);
    overrideClipId = null;
  }

  /** Pointer move during a drag: update the draft transform. */
  function onPointerMove(e: PointerEvent): void {
    const d = drag;
    if (d === null) return;
    const p = toCanvasPoint(e);
    if (p === null) return;
    const o = d.orig;
    if (d.mode === 'move') {
      setDraft(d.clipId, {
        ...o,
        pos_x: clampPos(o.pos_x + (p.x - d.start.x) / cw),
        pos_y: clampPos(o.pos_y + (p.y - d.start.y) / ch),
      });
      return;
    }
    const factor = scaleFactor(d.mode, d, p);
    setDraft(d.clipId, { ...o, scale: clampScale(o.scale * factor) });
  }

  /** Pointer up: commit the draft as one `patch_clips` edit when it changed. */
  function endDrag(): void {
    const d = drag;
    const dr = draft;
    drag = null;
    if (d === null || dr === null || dr.clipId !== d.clipId) {
      releaseDraft();
      return;
    }
    if (sameTransform(dr.transform, d.base)) {
      releaseDraft();
      return;
    }
    const pending = dispatch({
      type: 'patch_clips',
      ids: [d.clipId],
      patch: { transform: dr.transform },
    });
    // Keep the draft on screen until the edit lands, so the picture does not flash back.
    const done = (): void => {
      if (drag === null) releaseDraft();
    };
    void pending.then(done, done);
  }

  /** Arrow keys nudge the edit clip (Shift for a larger step). */
  function onStageKeydown(e: KeyboardEvent): void {
    const clip = editClip;
    if (clip === null) return;
    const step = e.shiftKey ? NUDGE_SHIFT : NUDGE;
    let dx = 0;
    let dy = 0;
    if (e.key === 'ArrowLeft') dx = -step;
    else if (e.key === 'ArrowRight') dx = step;
    else if (e.key === 'ArrowUp') dy = -step;
    else if (e.key === 'ArrowDown') dy = step;
    else return;
    e.preventDefault();
    e.stopPropagation();
    const t = toCustom(clip, clip.transform);
    void dispatch({
      type: 'patch_clips',
      ids: [clip.id],
      patch: {
        transform: {
          ...t,
          pos_x: clampPos(t.pos_x + dx),
          pos_y: clampPos(t.pos_y + dy),
        },
      },
    });
  }

  /** Resets the edit clip to the default transform. */
  function resetTransform(): void {
    const clip = editClip;
    if (clip === null) return;
    void dispatch({
      type: 'patch_clips',
      ids: [clip.id],
      patch: { transform: defaultTransform() },
    });
  }

  /** Play or pause through the shared action, so shuttle reset and loop start apply. */
  function togglePlay(): void {
    void runAction(ACTIONS.play_pause, PLAY_CONTEXT);
  }

  /** Stops playback and moves by `n` frames (negative = back). */
  function stepFrames(n: number): void {
    playing.set(false);
    const target = Math.max(0, get(playhead) + n * frameTicks);
    seek(snapToFrame(target, fps));
  }

  function goToStart(): void {
    playing.set(false);
    seek(0);
  }

  function goToEnd(): void {
    playing.set(false);
    seek(timelineEnd);
  }

  function toggleMute(): void {
    muted = !muted;
  }

  function toggleFullscreen(): void {
    const el = stageEl;
    if (el === undefined) return;
    if (document.fullscreenElement === el) {
      void document.exitFullscreen().catch(() => undefined);
      return;
    }
    el.requestFullscreen().catch((err: unknown) => {
      toast.set(`Fullscreen failed: ${String(err)}`);
    });
  }

  function dismissError(index: number): void {
    mediaErrors = mediaErrors.filter((_, i) => i !== index);
  }

  /**
   * Auto quality: after enough consecutive reports above the drop threshold, step one level down.
   * It never steps back up within the session. Manual quality settings ignore the reports.
   */
  function reportDropped(ratio: number): void {
    if (quality !== 'auto' || !(ratio > AUTO_DROP_RATIO)) {
      droppedStreak = 0;
      return;
    }
    droppedStreak += 1;
    if (droppedStreak < AUTO_DROP_STREAK) return;
    droppedStreak = 0;
    if (autoLevel === 'full') autoLevel = 'half';
    else if (autoLevel === 'half') autoLevel = 'quarter';
  }

  // Create the engine on mount; destroy it on unmount.
  onMount(() => {
    const el = canvasEl;
    if (el === undefined) return;
    const e = createPreviewEngine({ mediaUrl: (id) => editorApi.mediaUrl(id) });
    e.attach(el);
    const unsubs = [
      e.onTime((t) => {
        lastEngineTick = t;
        playhead.set(t);
        if (!get(playing)) return;
        const wa = get(project)?.work_area ?? null;
        if (get(loopInOut) && wa !== null && wa.end > wa.start && t >= wa.end) {
          lastEngineTick = wa.start;
          playhead.set(wa.start);
          e.seek(wa.start);
          if (!e.isPlaying()) e.play(get(shuttleRate));
          return;
        }
        // Also true for an empty timeline (end 0), so play stops at once there.
        if (t >= timelineEndTick(get(project))) playing.set(false);
      }),
      e.onEnded(() => {
        playing.set(false);
      }),
      e.onDroppedFrames(reportDropped),
      e.onLevels((levels) => {
        meterDb = levels.master;
      }),
      e.onError((message) => {
        mediaErrors = [...mediaErrors, message].slice(-5);
      }),
    ];
    engine = e;
    return () => {
      for (const u of unsubs) u();
      drag = null;
      draft = null;
      if (overrideClipId !== null) e.setTransformOverride(overrideClipId, null);
      overrideClipId = null;
      engine = null;
      e.destroy();
    };
  });

  // Push every snapshot (project and render plan) to the engine.
  $effect(() => {
    const e = engine;
    const s = $snapshot;
    if (e === null || s === null) return;
    e.setProject(s.project, s.plan);
  });

  // Play and pause follow the store; the shuttle rate sets the speed while playing.
  $effect(() => {
    const e = engine;
    const p = $playing;
    const r = $shuttleRate;
    if (e === null) return;
    if (p) e.play(r);
    else e.pause();
  });

  // Store moves that did not come from the engine seek it. Echoes within one frame are ignored.
  $effect(() => {
    const e = engine;
    const t = $playhead;
    if (e === null) return;
    untrack(() => {
      if (Math.abs(t - lastEngineTick) <= Math.max(1, frameTicks)) return;
      lastEngineTick = t;
      e.seek(t);
    });
  });

  $effect(() => {
    const e = engine;
    const v = muted ? 0 : volume;
    if (e === null) return;
    e.setMasterVolume(v);
  });

  $effect(() => {
    saveAudioPrefs(volume, muted);
  });

  $effect(() => {
    const e = engine;
    const q = effectiveQuality;
    if (e === null) return;
    e.setQuality(q);
  });

  // Keep the fullscreen flag in sync when the browser leaves fullscreen (Escape).
  function onFullscreenChange(): void {
    isFullscreen = stageEl !== undefined && document.fullscreenElement === stageEl;
  }

  /** Handle position style inside the transform box, as percentages. */
  function handleStyle(h: Handle): string {
    const f = HANDLE_FRACTION[h];
    return `left: ${f.x * 100}%; top: ${f.y * 100}%;`;
  }

  /** Box style of the transform overlay, as percentages of the picture box. */
  const boxStyle = $derived.by((): string | null => {
    const r = editRect;
    if (r === null) return null;
    return `left: ${(r.x / cw) * 100}%; top: ${(r.y / ch) * 100}%; width: ${(r.w / cw) * 100}%; height: ${(r.h / ch) * 100}%;`;
  });

  const timecode = $derived(formatTimecode($playhead, fps));
  const durationLabel = $derived(formatTimecode(timelineEnd, fps));
  const durationText = $derived(formatDuration(timelineEnd));
</script>

<svelte:document onfullscreenchange={onFullscreenChange} />

<section class="program-monitor" aria-label="Program monitor">
  <!-- Focusable region with its own key handling (arrow nudges), by design. -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <div
    class="stage"
    bind:this={stageEl}
    tabindex="0"
    role="application"
    aria-label="Program picture. Arrow keys nudge the selected clip."
    onkeydown={onStageKeydown}
  >
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="frame"
      bind:this={frameEl}
      style="width: min(100cqw, calc(100cqh * {cw} / {ch})); aspect-ratio: {frameRatio};"
      onpointerdown={onPointerDown}
      onpointermove={onPointerMove}
      onpointerup={endDrag}
      onpointercancel={endDrag}
    >
      <canvas bind:this={canvasEl} class="picture" aria-label="Program picture"></canvas>

      {#if showGuides}
        <div class="guides" aria-hidden="true">
          <div class="safe action"></div>
          <div class="safe title"></div>
          <svg viewBox="0 0 100 100" preserveAspectRatio="none">
            <line x1="33.333" y1="0" x2="33.333" y2="100" />
            <line x1="66.667" y1="0" x2="66.667" y2="100" />
            <line x1="0" y1="33.333" x2="100" y2="33.333" />
            <line x1="0" y1="66.667" x2="100" y2="66.667" />
          </svg>
        </div>
      {/if}

      {#if boxStyle !== null}
        <div class="xform" style={boxStyle} aria-hidden="true">
          {#each HANDLES as h (h)}
            <span class="handle" style={handleStyle(h)}></span>
          {/each}
        </div>
      {/if}
    </div>

    {#if mediaErrors.length > 0}
      <div class="strip" role="alert">
        {#each mediaErrors as msg, i (i)}
          <div class="strip-row">
            <span>{msg}</span>
            <button
              type="button"
              class="icon-btn"
              aria-label="Dismiss message"
              onclick={() => dismissError(i)}
            >
              <Icon name="close" size={14} />
            </button>
          </div>
        {/each}
      </div>
    {/if}
  </div>

  <div class="controls">
    <div class="row">
      <button type="button" class="btn" aria-label="Go to start" onclick={goToStart}>
        &#x23EE;
      </button>
      <button
        type="button"
        class="btn"
        aria-label="Step back one frame"
        onclick={() => stepFrames(-1)}
      >
        &#x2039;
      </button>
      <button
        type="button"
        class="btn primary"
        aria-label={$playing ? 'Pause' : 'Play'}
        aria-keyshortcuts="Space K"
        onclick={togglePlay}
      >
        <Icon name={$playing ? 'pause' : 'play'} size={16} />
      </button>
      <button
        type="button"
        class="btn"
        aria-label="Step forward one frame"
        onclick={() => stepFrames(1)}
      >
        &#x203A;
      </button>
      <button type="button" class="btn" aria-label="Go to end" onclick={goToEnd}>
        &#x23ED;
      </button>
      <span class="timecode" aria-label="Playhead">{timecode}</span>
      <span class="sep" aria-hidden="true">/</span>
      <span class="timecode dim" aria-label="Timeline length" title={durationLabel}>
        {durationText}
      </span>
      <button
        type="button"
        class="btn toggle"
        aria-pressed={$loopInOut}
        aria-label="Loop work area"
        title={canLoop ? 'Loop between in and out' : 'Set a work area first (I and O on the timeline)'}
        disabled={!canLoop}
        onclick={() => loopInOut.update((v) => !v)}
      >
        Loop
      </button>
    </div>

    <div class="row">
      <button
        type="button"
        class="btn icon"
        aria-label={muted ? 'Unmute' : 'Mute'}
        aria-pressed={muted}
        onclick={toggleMute}
      >
        <Icon name={muted || volume === 0 ? 'mute' : volume < 0.5 ? 'volume-low' : 'volume'} size={16} />
      </button>
      <input
        type="range"
        class="volume"
        min="0"
        max="1"
        step="0.01"
        value={volume}
        aria-label="Program volume"
        aria-valuetext={muted ? 'Muted' : `${Math.round(volume * 100)} percent`}
        oninput={(e) => {
          volume = clamp01(Number(e.currentTarget.value));
          if (muted && volume > 0) muted = false;
        }}
      />
      <div
        class="meter"
        role="meter"
        aria-label="Master level"
        aria-valuemin={METER_FLOOR_DB}
        aria-valuemax="0"
        aria-valuenow={Math.round(Math.max(METER_FLOOR_DB, meterDb))}
        aria-valuetext={`${Math.round(Math.max(METER_FLOOR_DB, meterDb))} dBFS`}
      >
        <div class="bar" style="width: {meterPercent(meterDb)}%"></div>
      </div>

      <label class="inline">
        <span class="dim">Quality</span>
        <select
          bind:value={quality}
          aria-label="Preview quality"
          onchange={() => (droppedStreak = 0)}
        >
          <option value="auto">Auto ({autoLevel})</option>
          <option value="full">Full</option>
          <option value="half">1/2</option>
          <option value="quarter">1/4</option>
        </select>
      </label>

      <button
        type="button"
        class="btn toggle"
        aria-pressed={showGuides}
        aria-label="Show safe area and grid"
        onclick={() => (showGuides = !showGuides)}
      >
        Guides
      </button>
      <button
        type="button"
        class="btn icon"
        aria-label={isFullscreen ? 'Exit fullscreen' : 'Fullscreen'}
        onclick={toggleFullscreen}
      >
        <Icon name={isFullscreen ? 'fullscreen-exit' : 'fullscreen'} size={16} />
      </button>
      <button
        type="button"
        class="btn"
        aria-label="Reset transform of the selected clip"
        disabled={editClip === null}
        onclick={resetTransform}
      >
        Reset transform
      </button>
    </div>
  </div>
</section>

<style>
  .program-monitor {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
    min-height: 0;
    height: 100%;
    padding: 8px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text);
  }

  .stage {
    position: relative;
    flex: 1 1 auto;
    min-height: 120px;
    min-width: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    container-type: size;
    background: #000;
    border-radius: var(--radius);
    outline: none;
    overflow: hidden;
  }

  .stage:fullscreen {
    border-radius: 0;
  }

  .stage:focus-visible {
    box-shadow: inset 0 0 0 2px var(--accent);
  }

  .frame {
    position: relative;
    max-width: 100%;
    max-height: 100%;
    touch-action: none;
  }

  .picture {
    display: block;
    width: 100%;
    height: 100%;
    background: #000;
  }

  .guides {
    position: absolute;
    inset: 0;
    pointer-events: none;
  }

  .guides svg {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    stroke: rgba(255, 255, 255, 0.35);
    stroke-width: 1;
    vector-effect: non-scaling-stroke;
  }

  .safe {
    position: absolute;
    border: 1px dashed rgba(255, 255, 255, 0.5);
  }

  .safe.action {
    inset: 5%;
  }

  .safe.title {
    inset: 10%;
  }

  .xform {
    position: absolute;
    border: 1px solid var(--accent);
    box-sizing: border-box;
    pointer-events: none;
  }

  .handle {
    position: absolute;
    width: 9px;
    height: 9px;
    margin: -4.5px 0 0 -4.5px;
    background: var(--surface);
    border: 1px solid var(--accent);
    border-radius: 2px;
  }

  .strip {
    position: absolute;
    left: 8px;
    right: 8px;
    top: 8px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .strip-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 6px 8px;
    background: rgba(0, 0, 0, 0.8);
    color: var(--danger);
    border-radius: 6px;
    font-size: 13px;
  }

  .controls {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px;
    min-width: 0;
  }

  .dim {
    color: var(--text-dim);
  }

  .sep {
    color: var(--text-dim);
  }

  .timecode {
    font-family: ui-monospace, Consolas, monospace;
    font-size: 13px;
    font-variant-numeric: tabular-nums;
  }

  .btn {
    min-height: 28px;
    min-width: 28px;
    padding: 0 8px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
    font-size: 14px;
  }

  .btn:hover:not(:disabled) {
    border-color: var(--text-dim);
  }

  .btn.primary {
    border-color: var(--accent);
  }

  .btn.toggle[aria-pressed='true'] {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  .btn:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .btn:focus-visible,
  .icon-btn:focus-visible,
  select:focus-visible,
  input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .icon-btn {
    width: 24px;
    height: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    border: none;
    border-radius: 4px;
    color: inherit;
    cursor: pointer;
    flex: 0 0 auto;
  }

  .volume {
    width: 110px;
    accent-color: var(--accent);
  }

  .meter {
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 90px;
  }

  .bar {
    height: 4px;
    background: linear-gradient(to right, #3ddc84 0%, #e6c229 80%, var(--accent) 100%);
    border-radius: 2px;
    max-width: 100%;
    transition: width 60ms linear;
  }

  .inline {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
  }

  select {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 3px 6px;
  }
</style>
