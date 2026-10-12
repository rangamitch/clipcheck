<script lang="ts">
  // Time ruler above the track area: tick marks and labels, the work area (in/out) band with
  // draggable ends, marker flags, and the playhead handle. Pointer down or drag seeks.
  import { onDestroy } from 'svelte';
  import { beginScrub, dispatch, endScrub, playhead } from './store';
  import {
    RULER_HEIGHT,
    formatRulerLabel,
    niceRulerStep,
    tickToX,
    xToTick,
    type View,
  } from './layout';
  import { ACCENT, readPalette } from './drawClips';
  import { TICKS_PER_SECOND, clampTick, clipEnd, frameDurationTicks } from './ticks';
  import type { Marker, Project, Tick, WorkArea } from './types';

  let { view, project, onseek }: { view: View; project: Project; onseek: (t: Tick) => void } =
    $props();

  /** Pointer distance from a work area end that grabs it, px. */
  const HANDLE_HIT_PX = 6;
  /** Pointer distance from a marker flag that hits it, px. */
  const MARKER_HIT_PX = 6;
  /** Height of the marker flag zone at the top of the ruler, px. */
  const MARKER_ZONE_PX = 12;
  /** Height of the work area bar at the bottom of the ruler, px. */
  const WORK_BAR_PX = 5;
  /** Smallest work area length the drag allows, in ticks (10 ms). */
  const MIN_AREA_TICKS = Math.round(TICKS_PER_SECOND / 100);
  /** Most tick marks drawn in one frame (guards against degenerate zoom values). */
  const MAX_TICKS = 2000;
  /** Colour of marker flags without their own colour. */
  const DEFAULT_MARKER_COLOR = '#f5c542';

  let canvas = $state<HTMLCanvasElement | null>(null);
  /** Work area while an end is being dragged; null otherwise. */
  let areaPreview = $state<WorkArea | null>(null);
  /** Which work area end is being dragged, if any. */
  let areaDrag: 'start' | 'end' | null = null;
  /** True while a pointer drag seeks the playhead. */
  let seeking = false;
  /** True while a scrub is open in the store; keeps beginScrub/endScrub paired once per drag. */
  let scrubActive = false;

  function startScrub(): void {
    if (scrubActive) return;
    scrubActive = true;
    beginScrub();
  }

  function stopScrub(): void {
    if (!scrubActive) return;
    scrubActive = false;
    endScrub();
  }

  onDestroy(stopScrub);
  /** Tick where an Alt- or Shift-drag on empty ruler started; null when not creating a work area. */
  let createAnchor: Tick | null = null;

  /** Rounds `t` down to a frame boundary of the project rate. */
  function frameFloor(t: Tick): Tick {
    const f = frameDurationTicks(project.canvas.fps);
    return f > 0 ? Math.floor(t / f) * f : t;
  }

  /** Rounds `t` up to a frame boundary of the project rate. */
  function frameCeil(t: Tick): Tick {
    const f = frameDurationTicks(project.canvas.fps);
    return f > 0 ? Math.ceil(t / f) * f : t;
  }

  /** End of the last clip, rounded up to a frame. A new work area may not extend past it. */
  function timelineEndTick(): Tick {
    let end = 0;
    for (const clip of project.clips) {
      end = Math.max(end, clipEnd(clip));
    }
    return frameCeil(end);
  }

  /** Work area spanned by a creation drag from `anchor` to `tick`, frame aligned and clamped. */
  function createdArea(anchor: Tick, tick: Tick): WorkArea {
    const limit = timelineEndTick();
    const a = clampTick(anchor, 0, limit);
    const b = clampTick(tick, 0, limit);
    return { start: frameFloor(Math.min(a, b)), end: frameCeil(Math.max(a, b)) };
  }

  $effect(() => {
    const c = canvas;
    if (!c) return;
    draw(c, view, project, areaPreview, $playhead);
  });

  /** Draws the whole ruler into `c` for the given view and state. */
  function draw(
    c: HTMLCanvasElement,
    v: View,
    p: Project,
    preview: WorkArea | null,
    head: Tick,
  ): void {
    const width = Math.max(0, v.width);
    const dpr = typeof window !== 'undefined' && window.devicePixelRatio > 0 ? window.devicePixelRatio : 1;
    // Assigning width or height resets the bitmap, so only do it when the size changed.
    const bitmapW = Math.max(1, Math.round(width * dpr));
    const bitmapH = Math.round(RULER_HEIGHT * dpr);
    if (c.width !== bitmapW) c.width = bitmapW;
    if (c.height !== bitmapH) c.height = bitmapH;
    c.style.width = `${width}px`;
    c.style.height = `${RULER_HEIGHT}px`;

    const ctx = c.getContext('2d');
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    const palette = readPalette(c);
    ctx.clearRect(0, 0, width, RULER_HEIGHT);
    ctx.fillStyle = palette.surface;
    ctx.fillRect(0, 0, width, RULER_HEIGHT);

    drawWorkArea(ctx, preview ?? p.work_area, v, width);
    drawTicks(ctx, v, width, palette.textDim);
    drawMarkers(ctx, p.markers, v, width);
    drawPlayheadHandle(ctx, head, v, width);
  }

  /** Work area band with bright end bars. */
  function drawWorkArea(
    ctx: CanvasRenderingContext2D,
    area: WorkArea | null,
    v: View,
    width: number,
  ): void {
    if (!area || area.end <= area.start) return;
    const x0 = Math.max(0, tickToX(area.start, v));
    const x1 = Math.min(width, tickToX(area.end, v));
    if (x1 <= x0) return;
    ctx.fillStyle = 'rgba(255, 255, 255, 0.1)';
    ctx.fillRect(x0, 0, x1 - x0, RULER_HEIGHT);
    ctx.fillStyle = 'rgba(255, 255, 255, 0.5)';
    ctx.fillRect(x0, RULER_HEIGHT - WORK_BAR_PX, x1 - x0, WORK_BAR_PX);
    ctx.fillStyle = 'rgba(255, 255, 255, 0.85)';
    ctx.fillRect(tickToX(area.start, v) - 1, 0, 3, RULER_HEIGHT);
    ctx.fillRect(tickToX(area.end, v) - 1, 0, 3, RULER_HEIGHT);
  }

  /** Major ticks with labels and minor ticks between them. */
  function drawTicks(ctx: CanvasRenderingContext2D, v: View, width: number, colour: string): void {
    const pps = v.pxPerSec > 0 && Number.isFinite(v.pxPerSec) ? v.pxPerSec : 1;
    const step = niceRulerStep(pps);
    const minor = step.minorSec;
    const majorEvery = Math.max(1, Math.round(step.majorSec / minor));
    const startSec = Math.max(0, v.scrollX / pps);
    const endSec = (v.scrollX + width) / pps;

    ctx.strokeStyle = colour;
    ctx.fillStyle = colour;
    ctx.lineWidth = 1;
    ctx.font = '10px system-ui, -apple-system, sans-serif';
    ctx.textBaseline = 'top';
    ctx.textAlign = 'left';

    const first = Math.floor(startSec / minor);
    for (let i = first, n = 0; n < MAX_TICKS; i += 1, n += 1) {
      const sec = i * minor;
      if (sec > endSec) break;
      const x = Math.round(tickToX(Math.round(sec * TICKS_PER_SECOND), v)) + 0.5;
      if (x < 0 || x > width) continue;
      const isMajor = i % majorEvery === 0;
      ctx.beginPath();
      ctx.moveTo(x, RULER_HEIGHT - (isMajor ? 10 : 5));
      ctx.lineTo(x, RULER_HEIGHT);
      ctx.stroke();
      if (isMajor) {
        ctx.fillText(formatRulerLabel(sec), x + 3, 2);
      }
    }
  }

  /** Marker flags along the top edge. */
  function drawMarkers(
    ctx: CanvasRenderingContext2D,
    markers: Marker[],
    v: View,
    width: number,
  ): void {
    for (const marker of markers) {
      const x = Math.round(tickToX(marker.time, v)) + 0.5;
      if (x < -10 || x > width + 10) continue;
      ctx.fillStyle = marker.color ?? DEFAULT_MARKER_COLOR;
      ctx.beginPath();
      ctx.moveTo(x, 0);
      ctx.lineTo(x + 8, 4);
      ctx.lineTo(x, 8);
      ctx.closePath();
      ctx.fill();
    }
  }

  /** Playhead: accent triangle on top and a line down the ruler. */
  function drawPlayheadHandle(
    ctx: CanvasRenderingContext2D,
    head: Tick,
    v: View,
    width: number,
  ): void {
    const x = Math.round(tickToX(head, v)) + 0.5;
    if (x < -6 || x > width + 6) return;
    ctx.strokeStyle = ACCENT;
    ctx.fillStyle = ACCENT;
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    ctx.moveTo(x - 6, 0);
    ctx.lineTo(x + 6, 0);
    ctx.lineTo(x, 8);
    ctx.closePath();
    ctx.fill();
    ctx.beginPath();
    ctx.moveTo(x, 8);
    ctx.lineTo(x, RULER_HEIGHT);
    ctx.stroke();
  }

  /** Pointer position relative to the canvas, in CSS px. */
  function localPoint(e: PointerEvent): { x: number; y: number } {
    const rect = canvas?.getBoundingClientRect();
    return { x: e.clientX - (rect?.left ?? 0), y: e.clientY - (rect?.top ?? 0) };
  }

  /** Work area end under (x, y), or null. The nearer end wins. */
  function handleAt(x: number, area: WorkArea | null): 'start' | 'end' | null {
    if (!area || area.end <= area.start) return null;
    const dStart = Math.abs(x - tickToX(area.start, view));
    const dEnd = Math.abs(x - tickToX(area.end, view));
    const startHit = dStart <= HANDLE_HIT_PX;
    const endHit = dEnd <= HANDLE_HIT_PX;
    if (startHit && endHit) return dStart <= dEnd ? 'start' : 'end';
    if (startHit) return 'start';
    if (endHit) return 'end';
    return null;
  }

  /** Marker whose flag is under (x, y), or null. */
  function markerAt(x: number, y: number): Marker | null {
    if (y > MARKER_ZONE_PX) return null;
    let best: Marker | null = null;
    let bestDist = Infinity;
    for (const marker of project.markers) {
      const d = Math.abs(x - tickToX(marker.time, view));
      if (d <= MARKER_HIT_PX && d < bestDist) {
        best = marker;
        bestDist = d;
      }
    }
    return best;
  }

  /** Moves one end of the work area preview to `tick`, keeping it a valid range. */
  function previewArea(handle: 'start' | 'end', tick: Tick): void {
    const base = areaPreview ?? project.work_area;
    if (!base) return;
    if (handle === 'start') {
      const start = Math.max(0, Math.min(tick, base.end - MIN_AREA_TICKS));
      areaPreview = { start, end: base.end };
    } else {
      const end = Math.max(tick, base.start + MIN_AREA_TICKS);
      areaPreview = { start: base.start, end };
    }
  }

  function onPointerDown(e: PointerEvent): void {
    // The ruler sits inside the timeline, whose own pointer handling must not also run.
    e.stopPropagation();
    if (e.button !== 0 || !canvas) return;
    const { x, y } = localPoint(e);
    canvas.setPointerCapture(e.pointerId);

    const handle = handleAt(x, project.work_area);
    if (handle) {
      areaDrag = handle;
      areaPreview = project.work_area;
      return;
    }
    if (e.altKey || e.shiftKey) {
      // Alt- or Shift-drag on empty ruler creates a work area on release.
      seeking = false;
      createAnchor = clampTick(xToTick(x, view), 0, timelineEndTick());
      areaPreview = null;
      return;
    }
    const marker = markerAt(x, y);
    if (marker) {
      seeking = false;
      onseek(marker.time);
      return;
    }
    seeking = true;
    startScrub();
    onseek(xToTick(x, view));
  }

  function onPointerMove(e: PointerEvent): void {
    const { x, y } = localPoint(e);
    if (createAnchor !== null) {
      areaPreview = createdArea(createAnchor, xToTick(x, view));
      return;
    }
    if (areaDrag) {
      previewArea(areaDrag, xToTick(x, view));
      return;
    }
    if (seeking) {
      onseek(xToTick(x, view));
      return;
    }
    if (canvas) {
      if (handleAt(x, project.work_area)) {
        canvas.style.cursor = 'ew-resize';
      } else if (markerAt(x, y)) {
        canvas.style.cursor = 'pointer';
      } else {
        canvas.style.cursor = 'default';
      }
    }
  }

  function onPointerUp(e: PointerEvent): void {
    if (canvas?.hasPointerCapture(e.pointerId)) {
      canvas.releasePointerCapture(e.pointerId);
    }
    seeking = false;
    stopScrub();
    if (createAnchor !== null) {
      const anchor = createAnchor;
      createAnchor = null;
      const created = areaPreview;
      if (created && created.end > created.start) {
        void dispatch({ type: 'set_work_area', area: created }).finally(() => {
          areaPreview = null;
        });
      } else {
        // A plain click (no drag) just moves the playhead.
        areaPreview = null;
        onseek(anchor);
      }
      return;
    }
    if (areaDrag) {
      areaDrag = null;
      const area = areaPreview;
      if (area) {
        // Keep the preview until the edit lands so the band does not flicker back.
        void dispatch({ type: 'set_work_area', area }).finally(() => {
          areaPreview = null;
        });
      } else {
        areaPreview = null;
      }
    }
  }

  /** The canvas lost pointer capture: a seek drag cannot continue, so close it. */
  function onLostCapture(): void {
    seeking = false;
    stopScrub();
  }

  /** Double click on a marker flag removes that marker. */
  function onDoubleClick(e: MouseEvent): void {
    const rect = canvas?.getBoundingClientRect();
    const marker = markerAt(e.clientX - (rect?.left ?? 0), e.clientY - (rect?.top ?? 0));
    if (marker) {
      void dispatch({ type: 'remove_marker', marker_id: marker.id });
    }
  }
</script>

<canvas
  bind:this={canvas}
  class="ruler"
  aria-label="Timeline ruler. Click or drag to move the playhead. Drag the ends of the highlighted band to set the work area, or Alt-drag (Shift-drag) on empty ruler to create one. Double click a marker flag to remove it."
  onpointerdown={onPointerDown}
  onpointermove={onPointerMove}
  onpointerup={onPointerUp}
  onpointercancel={onPointerUp}
  onlostpointercapture={onLostCapture}
  ondblclick={onDoubleClick}
></canvas>

<style>
  .ruler {
    display: block;
    box-sizing: border-box;
    touch-action: none;
    cursor: default;
    border-bottom: 1px solid var(--border);
  }
</style>
