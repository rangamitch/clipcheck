<script lang="ts">
  // Medal-style clip timeline: time ruler, filmstrip, dimmed area outside the
  // kept range, draggable trim handles and a playhead. Long clips can be zoomed
  // (1x..64x) and scrolled horizontally. Pointer and keyboard handling only; the
  // maths lives in timeline.ts and trim.ts.
  import { tick, untrack } from 'svelte';
  import Icon from './Icon.svelte';
  import { formatTime } from './model';
  import {
    clampZoom,
    isTrimmed,
    maxZoom,
    msToX,
    tickStep,
    visibleMsRange,
    xToMs,
    zoomAnchorScroll,
  } from './timeline';
  import {
    COARSE_MS,
    FRAME_MS,
    formatHmsMs,
    nudge,
    setIn,
    setOut,
    type HandleKind,
    type TrimSelection,
  } from './trim';

  interface Props {
    durationMs: number;
    nowMs: number;
    selection: TrimSelection;
    /** Data URLs in index order. '' or undefined means the frame is not loaded yet. */
    frames: string[];
    /** Seek that the player keeps inside the SOS..EOS range. */
    onseek: (ms: number) => void;
    /** Seek while a scrub is held: may go outside SOS..EOS (the release snaps back). */
    onseekfree?: (ms: number) => void;
    /** True while the pointer is held on the timeline or a handle. */
    onscrub?: (active: boolean) => void;
    onselection: (sel: TrimSelection, kind: HandleKind) => void;
    /** Called (debounced) after the zoom level changes, with the viewport width in px. */
    onzoom?: (zoom: number, visibleWidthPx: number) => void;
  }

  let { durationMs, nowMs, selection, frames, onseek, onseekfree, onscrub, onselection, onzoom }: Props = $props();

  /** Upper bound on drawn ruler ticks, so a huge clip cannot flood the DOM. */
  const MAX_TICKS = 2000;

  let wrapperEl: HTMLDivElement | undefined = $state();
  let scrollEl: HTMLDivElement | undefined = $state();
  let contentEl: HTMLDivElement | undefined = $state();
  /** Width of the visible viewport (the scroll container). */
  let containerWidth = $state(0);
  let scrollLeft = $state(0);
  let zoom = $state(1);
  let scrubbing = $state(false);
  let dragging = $state<HandleKind | null>(null);
  let focusedHandle = $state<HandleKind | null>(null);

  const zoomCap = $derived(maxZoom(durationMs));
  const canZoom = $derived(zoomCap > 1);
  /** Width of the whole zoomed strip; all geometry uses this. */
  const width = $derived(containerWidth * zoom);

  // A different clip can lower the cap below the current zoom.
  $effect(() => {
    if (zoom > zoomCap) {
      zoom = clampZoom(zoom, durationMs);
    }
  });

  const usable = $derived(Number.isFinite(durationMs) && durationMs > 0 && width > 0);
  const step = $derived(tickStep(durationMs, width));
  const startX = $derived(msToX(selection.startMs, width, durationMs));
  const endX = $derived(msToX(selection.endMs, width, durationMs));
  // The playhead stays in the kept range, except while the timeline is held outside it.
  const playX = $derived(
    msToX(
      scrubbing ? nowMs : Math.min(Math.max(nowMs, selection.startMs), selection.endMs),
      width,
      durationMs,
    ),
  );
  const trimmed = $derived(isTrimmed(selection, durationMs));
  const keptSec = $derived(Math.round((selection.endMs - selection.startMs) / 1000));
  const keptLabel = $derived(
    `${formatTime(selection.startMs / 1000)} - ${formatTime(selection.endMs / 1000)} (${keptSec}s)`,
  );
  /** Kept-range label sits in the ruler just right of the in handle, only when it fits. */
  const showKeptLabel = $derived(trimmed && usable && width - startX >= 130);

  const ticks = $derived.by(() => {
    const out: { ms: number; major: boolean }[] = [];
    if (!usable || step.minorMs <= 0) {
      return out;
    }
    // Only the ticks within one screen of the viewport are drawn.
    const range = visibleMsRange(durationMs, width, scrollLeft, containerWidth, 1);
    const first = Math.floor(range.fromMs / step.minorMs) * step.minorMs;
    for (let ms = first; ms <= range.toMs && out.length < MAX_TICKS; ms += step.minorMs) {
      out.push({ ms, major: ms % step.majorMs === 0 });
    }
    return out;
  });

  /** One placeholder tile when there are no frames yet, so the strip still shows. */
  const tiles = $derived(frames.length > 0 ? frames : ['']);

  function msFromClientX(clientX: number): number {
    // The content element moves with the scroll, so its rect already accounts for it.
    const rect = contentEl?.getBoundingClientRect();
    if (!rect || rect.width <= 0) {
      return 0;
    }
    return xToMs(clientX - rect.left, rect.width, durationMs);
  }

  /** Scrolls the viewport a little when the pointer is within 32 px of its left/right edge. */
  function edgeScroll(clientX: number): void {
    if (!scrollEl || zoom <= 1) {
      return;
    }
    const r = scrollEl.getBoundingClientRect();
    if (clientX < r.left + 32) {
      scrollEl.scrollLeft -= 12;
    } else if (clientX > r.right - 32) {
      scrollEl.scrollLeft += 12;
    }
  }

  function onScroll(): void {
    if (scrollEl) {
      scrollLeft = scrollEl.scrollLeft;
    }
  }

  // ---- Zoom ----

  /** Time and viewport x of the playhead when it is on screen, otherwise of the viewport centre. */
  function buttonAnchor(): { ms: number; x: number } {
    const px = playX - scrollLeft;
    if (px >= 0 && px <= containerWidth) {
      return { ms: Math.min(Math.max(nowMs, 0), durationMs), x: px };
    }
    const x = containerWidth / 2;
    return { ms: xToMs(scrollLeft + x, width, durationMs), x };
  }

  /** Changes the zoom level and keeps `anchorMs` at `anchorX` (px in the viewport). */
  async function applyZoom(next: number, anchorMs: number, anchorX: number): Promise<void> {
    const z = clampZoom(next, durationMs);
    if (z === zoom || !usable) {
      return;
    }
    zoom = z;
    await tick();
    if (!scrollEl) {
      return;
    }
    scrollEl.scrollLeft = zoomAnchorScroll(anchorMs, anchorX, containerWidth * z, durationMs);
    scrollLeft = scrollEl.scrollLeft;
  }

  function zoomBy(factor: number): void {
    const a = buttonAnchor();
    void applyZoom(zoom * factor, a.ms, a.x);
  }

  let wheelAcc = 0;

  // Ctrl+wheel zooms around the pointer. Needs a non-passive listener to cancel the page zoom.
  $effect(() => {
    const el = wrapperEl;
    if (!el) {
      return;
    }
    const handler = (e: WheelEvent) => {
      if (!e.ctrlKey) {
        return;
      }
      e.preventDefault();
      if (!canZoom || !scrollEl) {
        return;
      }
      wheelAcc += e.deltaY;
      if (Math.abs(wheelAcc) < 40) {
        return;
      }
      const factor = wheelAcc < 0 ? 2 : 0.5;
      wheelAcc = 0;
      const ms = msFromClientX(e.clientX);
      void applyZoom(zoom * factor, ms, e.clientX - scrollEl.getBoundingClientRect().left);
    };
    el.addEventListener('wheel', handler, { passive: false });
    return () => el.removeEventListener('wheel', handler);
  });

  // Tell the player (debounced) once the zoom level settled, so it can redraw the filmstrip.
  let notifiedZoom = 1;
  $effect(() => {
    const z = zoom;
    if (z === notifiedZoom) {
      return;
    }
    const timer = setTimeout(() => {
      notifiedZoom = z;
      onzoom?.(z, untrack(() => containerWidth));
    }, 500);
    return () => clearTimeout(timer);
  });

  // Follow the playhead while it plays, unless the user scrolled by hand a moment ago.
  let userScrollAt = 0;
  function markUserScroll(): void {
    userScrollAt = Date.now();
  }
  $effect(() => {
    void nowMs;
    untrack(() => {
      if (!scrollEl || zoom <= 1 || scrubbing || dragging !== null) {
        return;
      }
      if (Date.now() - userScrollAt < 2500) {
        return;
      }
      if (playX < scrollLeft || playX > scrollLeft + containerWidth) {
        scrollEl.scrollLeft = Math.max(0, playX - containerWidth * 0.2);
      }
    });
  });

  // ---- Pointer and keyboard ----

  function onTimelineDown(e: PointerEvent): void {
    if (e.button !== 0 || !usable) {
      return;
    }
    e.preventDefault();
    scrubbing = true;
    onscrub?.(true);
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    scrubMs = msFromClientX(e.clientX);
    (onseekfree ?? onseek)(scrubMs);
  }

  function onTimelineMove(e: PointerEvent): void {
    if (!scrubbing) {
      return;
    }
    edgeScroll(e.clientX);
    scrubMs = msFromClientX(e.clientX);
    (onseekfree ?? onseek)(scrubMs);
  }

  /** Releasing outside the kept range snaps the playhead to the nearest bound. */
  function onTimelineUp(): void {
    if (!scrubbing) {
      return;
    }
    scrubbing = false;
    onscrub?.(false);
    onseek(Math.min(Math.max(scrubMs, selection.startMs), selection.endMs));
  }

  /** Playhead distance kept before the EOS after a drag ends. */
  const END_PREVIEW_MS = 5000;
  let handleMoved = false;
  /** Last pointer time of the timeline scrub in progress. */
  let scrubMs = 0;

  function onHandleDown(kind: HandleKind, e: PointerEvent): void {
    if (e.button !== 0 || !usable) {
      return;
    }
    e.preventDefault();
    e.stopPropagation();
    const el = e.currentTarget as HTMLElement;
    el.focus();
    focusedHandle = kind;
    dragging = kind;
    handleMoved = false;
    onscrub?.(true);
    el.setPointerCapture(e.pointerId);
  }

  function onHandleMove(kind: HandleKind, e: PointerEvent): void {
    if (dragging !== kind) {
      return;
    }
    e.stopPropagation();
    handleMoved = true;
    edgeScroll(e.clientX);
    const ms = msFromClientX(e.clientX);
    const next = kind === 'start' ? setIn(selection, ms, durationMs) : setOut(selection, ms, durationMs);
    onselection(next, kind);
    onseek(kind === 'start' ? next.startMs : next.endMs);
  }

  function onHandleUp(e: PointerEvent): void {
    e.stopPropagation();
    const released = dragging;
    dragging = null;
    if (released !== null) {
      onscrub?.(false);
    }
    // Letting go of the EOS parks the playhead END_PREVIEW_MS before it (not before the SOS), so the
    // end of the clip can be checked straight away.
    if (released === 'end' && handleMoved) {
      onseek(Math.max(selection.startMs, selection.endMs - END_PREVIEW_MS));
    }
    handleMoved = false;
  }

  function onHandleKey(kind: HandleKind, e: KeyboardEvent): void {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') {
      return;
    }
    e.preventDefault();
    const dir = e.key === 'ArrowLeft' ? -1 : 1;
    const next = nudge(selection, kind, dir * (e.shiftKey ? COARSE_MS : FRAME_MS), durationMs);
    onselection(next, kind);
    onseek(kind === 'start' ? next.startMs : next.endMs);
  }
</script>

<div class="timeline" class:disabled={!usable} bind:this={wrapperEl}>
  {#if canZoom}
    <div class="zoombar">
      <button type="button" class="zbtn" aria-label="Zoom out" disabled={zoom <= 1} onclick={() => zoomBy(0.5)}>
        <Icon name="zoom-out" size={14} />
      </button>
      <button
        type="button"
        class="zlabel"
        title="Fit to view"
        aria-label="Fit timeline to view"
        onclick={() => zoomBy(1 / zoom)}>{zoom * 100}%</button
      >
      <button type="button" class="zbtn" aria-label="Zoom in" disabled={zoom >= zoomCap} onclick={() => zoomBy(2)}>
        <Icon name="zoom-in" size={14} />
      </button>
    </div>
  {/if}

  <div
    class="scroller"
    bind:this={scrollEl}
    bind:clientWidth={containerWidth}
    role="group"
    aria-label="Timeline"
    onscroll={onScroll}
    onwheel={markUserScroll}
    onpointerdown={markUserScroll}
  >
    <div
      class="content"
      bind:this={contentEl}
      style="width: {zoom === 1 ? '100%' : `${width}px`}"
      role="group"
      aria-label="Clip timeline"
      onpointerdown={onTimelineDown}
      onpointermove={onTimelineMove}
      onpointerup={onTimelineUp}
      onpointercancel={onTimelineUp}
    >
      <div class="ruler">
        {#each ticks as tick (tick.ms)}
          <div class="tick" class:major={tick.major} style="left: {msToX(tick.ms, width, durationMs)}px"></div>
          {#if tick.major}
            <span class="tick-label" style="left: {msToX(tick.ms, width, durationMs)}px">{formatTime(tick.ms / 1000)}</span>
          {/if}
        {/each}
        {#if showKeptLabel}
          <span class="kept-label" style="left: {startX + 8}px">{keptLabel}</span>
        {/if}
      </div>

      <div class="track">
        <div class="strip">
          {#each tiles as src, i (i)}
            <div class="tile">
              {#if src}
                <img {src} alt="" draggable="false" />
              {/if}
            </div>
          {/each}
        </div>

        <div class="shade" style="left: 0px; width: {Math.max(0, startX)}px"></div>
        <div class="shade" style="left: {Math.max(0, endX)}px; right: 0"></div>

        <div class="range" style="left: {startX}px; width: {Math.max(0, endX - startX)}px"></div>

        <div
          class="handle in"
          class:active={dragging === 'start' || focusedHandle === 'start'}
          style="left: {startX}px"
          role="slider"
          tabindex="0"
          aria-label="Trim start"
          aria-orientation="horizontal"
          aria-valuemin={0}
          aria-valuemax={Math.round(durationMs)}
          aria-valuenow={Math.round(selection.startMs)}
          aria-valuetext={formatHmsMs(selection.startMs)}
          onpointerdown={(e) => onHandleDown('start', e)}
          onpointermove={(e) => onHandleMove('start', e)}
          onpointerup={onHandleUp}
          onpointercancel={onHandleUp}
          onkeydown={(e) => onHandleKey('start', e)}
          onfocus={() => (focusedHandle = 'start')}
          onblur={() => (focusedHandle = focusedHandle === 'start' ? null : focusedHandle)}
        ><span class="grip"></span></div>

        <div
          class="handle out"
          class:active={dragging === 'end' || focusedHandle === 'end'}
          style="left: {endX - 10}px"
          role="slider"
          tabindex="0"
          aria-label="Trim end"
          aria-orientation="horizontal"
          aria-valuemin={0}
          aria-valuemax={Math.round(durationMs)}
          aria-valuenow={Math.round(selection.endMs)}
          aria-valuetext={formatHmsMs(selection.endMs)}
          onpointerdown={(e) => onHandleDown('end', e)}
          onpointermove={(e) => onHandleMove('end', e)}
          onpointerup={onHandleUp}
          onpointercancel={onHandleUp}
          onkeydown={(e) => onHandleKey('end', e)}
          onfocus={() => (focusedHandle = 'end')}
          onblur={() => (focusedHandle = focusedHandle === 'end' ? null : focusedHandle)}
        ><span class="grip"></span></div>
      </div>

      {#if usable}
        <div class="playhead" style="left: {playX}px" aria-hidden="true">
          <span class="flag"></span>
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .timeline {
    --scrim: rgba(0, 0, 0, 0.55);
    --green: #7ee05a;
    --handle-w: 10px;
    position: relative;
    width: 100%;
    user-select: none;
    cursor: pointer;
    color: var(--text-dim);
  }

  .timeline.disabled {
    opacity: 0.5;
    pointer-events: none;
  }

  .zoombar {
    display: flex;
    justify-content: flex-end;
    align-items: center;
    gap: 4px;
    margin-bottom: 4px;
  }

  .zbtn,
  .zlabel {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    height: 24px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--surface-2);
    color: var(--text);
    cursor: pointer;
    font: inherit;
    font-size: 12px;
    font-variant-numeric: tabular-nums;
  }

  .zbtn {
    width: 26px;
    padding: 0;
  }

  .zlabel {
    min-width: 48px;
    padding: 0 8px;
  }

  .zbtn:hover:not(:disabled),
  .zlabel:hover {
    border-color: var(--text-dim);
  }

  .zbtn:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .zbtn:focus-visible,
  .zlabel:focus-visible {
    outline: 2px solid var(--green);
    outline-offset: 1px;
  }

  /* Always scroll (never auto): the thin track is reserved even at 1x, so zooming in does not
     add a scrollbar and shift the layout. With no overflow there is no thumb and the track is transparent. */
  /* Padding-bottom sits between the track and the bar, so the bar is not crowded against the clips. */
  .scroller {
    overflow-x: scroll;
    overflow-y: hidden;
    padding-bottom: 6px;
    scrollbar-width: thin;
    scrollbar-color: #e8ecf2 transparent;
  }

  .content {
    position: relative;
    touch-action: none;
  }

  .ruler {
    position: relative;
    height: 24px;
    border-bottom: 1px solid var(--border);
    overflow: hidden;
  }

  .tick {
    position: absolute;
    bottom: 0;
    width: 1px;
    height: 5px;
    background: var(--border);
  }

  .tick.major {
    height: 10px;
    background: var(--text-dim);
  }

  .tick-label {
    position: absolute;
    top: 2px;
    transform: translateX(-50%);
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    color: var(--text-dim);
  }

  .kept-label {
    position: absolute;
    top: 3px;
    padding: 0 6px;
    border-radius: 4px;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--border);
  }

  .track {
    position: relative;
    height: 56px;
    margin-top: 4px;
    border-radius: var(--radius);
    overflow: hidden;
    background: var(--surface);
  }

  .strip {
    position: absolute;
    inset: 0;
    display: flex;
  }

  .tile {
    flex: 1 1 0;
    min-width: 0;
    height: 100%;
    background: var(--surface-2);
    overflow: hidden;
  }

  .tile img {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: cover;
    pointer-events: none;
  }

  .shade {
    position: absolute;
    top: 0;
    bottom: 0;
    background: var(--scrim);
    pointer-events: none;
  }

  .range {
    position: absolute;
    top: 0;
    bottom: 0;
    border-top: 2px solid #fff;
    border-bottom: 2px solid #fff;
    pointer-events: none;
  }

  .handle {
    position: absolute;
    top: 0;
    bottom: 0;
    width: var(--handle-w);
    border-radius: 6px;
    background: #fff;
    cursor: ew-resize;
    outline: none;
    z-index: 2;
  }

  .handle.in {
    border-top-right-radius: 0;
    border-bottom-right-radius: 0;
  }

  .handle.out {
    border-top-left-radius: 0;
    border-bottom-left-radius: 0;
  }

  .handle.active,
  .handle:focus-visible {
    background: var(--green);
  }

  .grip {
    position: absolute;
    left: 50%;
    top: 30%;
    bottom: 30%;
    width: 2px;
    margin-left: -1px;
    border-radius: 1px;
    background: rgba(0, 0, 0, 0.45);
    pointer-events: none;
  }

  .playhead {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    margin-left: -1px;
    background: var(--green);
    pointer-events: none;
    z-index: 3;
  }

  .flag {
    position: absolute;
    top: 0;
    left: 50%;
    width: 0;
    height: 0;
    transform: translateX(-50%);
    border-left: 5px solid transparent;
    border-right: 5px solid transparent;
    border-top: 7px solid var(--green);
  }
</style>
