<script lang="ts">
  // Editor timeline: ruler, track headers and one canvas for the clips (drawn by drawClips.ts).
  // Pointer, wheel and drag-and-drop handling only. Edits go through the store `dispatch`; the
  // maths of a drag lives in timelineDrag.ts. Keyboard shortcuts belong to Editor.svelte.
  import { onDestroy } from 'svelte';
  import { get } from 'svelte/store';
  import TrackHeader from './TrackHeader.svelte';
  import Ruler from './Ruler.svelte';
  import ContextMenu, { type MenuItem } from './ContextMenu.svelte';
  import { drawTimeline, ensureMediaAssets, type DrawArgs } from './drawClips';
  import { getPeaks, getThumbStrip } from './mediaCache';
  import { editorApi } from './api';
  import {
    binDrag,
    copySelection,
    dispatch,
    dispatchWith,
    expandWithGroups,
    isBusy,
    pasteAtPlayhead,
    beginScrub,
    endScrub,
    playhead,
    project as projectStore,
    pxPerSec,
    scrollX,
    scrollY,
    seek,
    selectAllClips,
    selectClips,
    selection,
    setScrollLimit,
    timelineEndTick,
    toast,
    tool,
    zoomBy,
    zoomToFit,
  } from './store';
  import { setDragCanceller } from './actions';
  import {
    RULER_HEIGHT,
    TRACK_GAP,
    clipRect,
    cutPreviewFor,
    cutSnapPoint,
    hitTest,
    snapTargets,
    snapTick,
    tickToX,
    trackLayout,
    xToTick,
    type HitResult,
    type TrackRow,
    type View,
  } from './layout';
  import {
    DRAG_THRESHOLD_PX,
    beginDrag,
    beginEnvelopeDrag,
    beginEnvelopeHitDrag,
    beginFadeDrag,
    beginPan,
    finishDrag,
    hitEnvelopePoint,
    movingClips,
    panScroll,
    shiftDragOrigin,
    updateDrag,
    type DragContext,
    type DragGhost,
    type DragState,
    type MarqueeRect,
    type PointerLike,
  } from './timelineDrag';
  import { clipEnd, secToTick, tickToSec } from './ticks';
  import type { ApplyReply, Clip, Command, MediaRef, Project, Tick, Track, TrackKind } from './types';

  /** Width of the track header column, px. */
  const HEADER_WIDTH = 140;
  /** Snap threshold, px (design doc F-TL-10). */
  const SNAP_PX = 8;
  /** Zoom factor per Ctrl+wheel notch. */
  const WHEEL_ZOOM = 1.15;
  /** Distance from the viewport edge that starts auto-scroll during a drag, px. */
  const EDGE_SCROLL_PX = 28;
  /** Auto-scroll speed per animation frame, px. */
  const AUTOSCROLL_PX = 14;
  /** Empty time kept scrollable after the last clip, in seconds. */
  const SCROLL_PAD_S = 60;
  /** Length of a freeze frame inserted at the playhead, in seconds. */
  const FREEZE_S = 2;
  /** Fade handles are hit within this many px horizontally of the fade apex. */
  const FADE_HIT_PX = 10;
  /** Fade handles sit in the top this many px of an audio clip. */
  const FADE_ZONE_PX = 8;
  /**
   * On a selected audio clip, a press this close to a fade handle (and in the fade zone) takes the
   * handle over the trim edge, when the fade is longer than 0 or the press is below the clip top.
   */
  const FADE_EDGE_PREF_PX = 6;
  /** Depth below the clip top (px) a press needs to take a zero-length fade over a trim edge. */
  const FADE_EDGE_MIN_DEPTH_PX = 2;
  /** Audio clips narrower than this many px have no fade handles. */
  const FADE_MIN_CLIP_PX = 60;
  /** Drag-and-drop payload type for recordings dragged from the recordings list. */
  const RECORDING_MIME = 'application/x-clipcheck-recording';

  /** Timeline area size and horizontal scroll bounds. */
  let viewportEl: HTMLDivElement | undefined = $state();
  let headersEl: HTMLDivElement | undefined = $state();
  let canvasEl: HTMLCanvasElement | undefined = $state();
  let vpWidth = $state(0);
  let vpHeight = $state(0);
  /** Bumped when media thumbnails or waveforms finish loading, to redraw. */
  let assetVersion = $state(0);
  /** Bumped when the device pixel ratio changes (window moved between screens, zoom), to redraw. */
  let dprVersion = $state(0);

  /** Active drag, or null. Replaced (not mutated) on each update. */
  let drag = $state.raw<DragState | null>(null);
  /** What the overlays show for the active drag. */
  interface Frame {
    ghosts: DragGhost[];
    snapTick: Tick | null;
    marquee: MarqueeRect | null;
    /** Live fade lengths while a fade handle is dragged, or null. */
    fadePreview: { clipId: string; fadeIn: Tick; fadeOut: Tick } | null;
  }
  const EMPTY_FRAME: Frame = { ghosts: [], snapTick: null, marquee: null, fadePreview: null };
  let frame = $state.raw<Frame>(EMPTY_FRAME);
  /** Drop target while a media-bin item or recording is dragged over the timeline. */
  let dropPreview = $state.raw<{ tick: Tick; row: TrackRow | null } | null>(null);
  /** Open context menu, or null. */
  let menu = $state.raw<{ x: number; y: number; items: MenuItem[] } | null>(null);
  /** Gap picked by a click or right-click (cleared by any other press, a selection, or Escape). */
  let selectedGap = $state.raw<{ trackId: string; start: Tick; end: Tick } | null>(null);
  /** Hit under the idle pointer, kept only for edges and gaps (the things a press would act on). */
  let hover = $state.raw<HitResult | null>(null);
  /** Pointer position over the timeline while the Cut tool is active, or null (drives the cut preview). */
  let cutPos = $state.raw<{ x: number; y: number } | null>(null);
  /** Shift state while the Cut tool is active: Shift turns cut snapping off. */
  let shiftHeld = $state(false);

  // Plain (non-reactive) pointer bookkeeping for the active press.
  let pointerId = -1;
  let pressX = 0;
  let pressY = 0;
  let armed = false;
  /** Clip to select alone when a press on a multi-selection ends without a drag. */
  let collapseTo: string | null = null;
  /** Playhead when a ruler drag began, restored by Escape. */
  let playheadAtPress: Tick | null = null;
  let rafDraw = 0;
  let rafScroll = 0;
  let scrollDir = 0;
  let lastPointer: PointerLike | null = null;

  const proj = $derived($projectStore);
  const rows = $derived(proj ? trackLayout(proj, $scrollY) : []);
  const trackById = $derived(new Map<string, Track>((proj?.tracks ?? []).map((t): [string, Track] => [t.id, t])));
  const view: View = $derived({
    pxPerSec: $pxPerSec,
    scrollX: $scrollX,
    width: vpWidth,
    height: vpHeight,
    headerWidth: HEADER_WIDTH,
  });

  /** Height of all track rows when unscrolled, including the ruler. */
  const contentHeight = $derived.by(() => {
    if (!proj) return RULER_HEIGHT;
    const flat = trackLayout(proj, 0);
    const last = flat[flat.length - 1];
    return last ? last.top + last.height + TRACK_GAP : RULER_HEIGHT;
  });
  const maxScrollY = $derived(Math.max(0, contentHeight - vpHeight));
  const maxScrollX = $derived.by(() => {
    const end = proj ? tickToSec(timelineEndTick(proj)) : 0;
    return Math.max(0, (end + SCROLL_PAD_S) * $pxPerSec - vpWidth);
  });

  // Horizontal scrollbar and zoom slider (bottom bar).
  const ZOOM_MIN = 0.05;
  const ZOOM_MAX = 4000;
  let trackEl: HTMLDivElement | undefined = $state();
  let trackW = $state(0);
  $effect(() => {
    if (!trackEl) return;
    trackW = trackEl.clientWidth;
    const ro = new ResizeObserver(() => {
      trackW = trackEl?.clientWidth ?? 0;
    });
    ro.observe(trackEl);
    return () => ro.disconnect();
  });
  const contentW = $derived(maxScrollX + vpWidth);
  const thumbWidth = $derived(
    contentW > 0 ? Math.min(trackW, Math.max(24, (vpWidth / contentW) * trackW)) : trackW,
  );
  const thumbLeft = $derived(
    maxScrollX > 0 ? ($scrollX / maxScrollX) * Math.max(0, trackW - thumbWidth) : 0,
  );
  let thumbDrag: { x: number; scroll: number } | null = null;

  function zoomToSlider(px: number): number {
    const f = Math.log(Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, px)) / ZOOM_MIN) / Math.log(ZOOM_MAX / ZOOM_MIN);
    return Math.round(f * 1000);
  }
  function onZoomSlider(e: Event): void {
    const v = Number((e.currentTarget as HTMLInputElement).value);
    const target = ZOOM_MIN * Math.pow(ZOOM_MAX / ZOOM_MIN, v / 1000);
    zoomBy(target / get(pxPerSec), xToTick(vpWidth / 2, view));
  }
  function onThumbDown(e: PointerEvent): void {
    if (maxScrollX <= 0) return;
    e.stopPropagation();
    thumbDrag = { x: e.clientX, scroll: get(scrollX) };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function onThumbMove(e: PointerEvent): void {
    if (!thumbDrag) return;
    const room = Math.max(1, trackW - thumbWidth);
    scrollX.set(clampX(thumbDrag.scroll + ((e.clientX - thumbDrag.x) / room) * maxScrollX));
  }
  function onThumbUp(): void {
    thumbDrag = null;
  }
  function onTrackDown(e: PointerEvent): void {
    if (maxScrollX <= 0 || e.target !== e.currentTarget) return;
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const dir = e.clientX - rect.left < thumbLeft ? -1 : 1;
    scrollX.set(clampX(get(scrollX) + dir * vpWidth * 0.9));
  }

  /** Edge under the pointer (idle, select tool) or being trimmed, for the cursor and the edge handle. */
  const hoverEdge = $derived.by((): { clipId: string; edge: 'start' | 'end' } | null => {
    if (drag !== null) {
      if (drag.kind === 'trim-start') return { clipId: drag.clipId, edge: 'start' };
      if (drag.kind === 'trim-end') return { clipId: drag.clipId, edge: 'end' };
      return null;
    }
    if ($tool !== 'select' || hover === null || hover.clipId === undefined) return null;
    if (hover.kind === 'edge-start') return { clipId: hover.clipId, edge: 'start' };
    if (hover.kind === 'edge-end') return { clipId: hover.clipId, edge: 'end' };
    return null;
  });

  const cursorClass = $derived.by(() => {
    if (drag?.kind === 'pan' || $tool === 'hand') return 'pan';
    if (drag?.kind === 'move') return 'moving';
    if (drag?.kind === 'trim-start' || drag?.kind === 'trim-end') return 'trim';
    if (drag?.kind === 'roll') return 'roll';
    if (drag !== null) return '';
    if ($tool === 'razor') return 'cut';
    if ($tool === 'zoom') return 'zoom';
    if (hoverEdge !== null) return 'trim';
    if ($tool === 'select' && hover?.kind === 'gap') return 'gap';
    return '';
  });

  // Any selection change ends the gap selection (a gap is only selected while no clip is).
  $effect(() => {
    if ($selection.length > 0) selectedGap = null;
  });
  // Any project change (undo, redo, edits) ends the gap selection, since the gap may be gone.
  $effect(() => {
    void proj;
    selectedGap = null;
  });

  const playX = $derived(tickToX($playhead, view));
  const playVisible = $derived(proj !== null && playX >= 0 && playX <= vpWidth);

  /**
   * Cut preview (Cut tool only): shown only over a clip, at the snapped cut point (playhead or clip
   * boundary within SNAP_PX; Shift disables snapping). Null when the tool is off or the pointer is
   * not over a clip. The click that cuts uses the same `cutSnapPoint` (layout.ts).
   */
  const cutPreview = $derived.by(() => {
    if ($tool !== 'razor' || cutPos === null || proj === null) return null;
    return cutPreviewFor(proj, rows, view, cutPos.x, cutPos.y, shiftHeld, $playhead, SNAP_PX);
  });

  // Cut tool: Shift is tracked on the window (not only on pointer events) so the preview reacts to
  // Shift without the mouse moving. Listeners exist only while the Cut tool is active.
  $effect(() => {
    if ($tool !== 'razor') {
      cutPos = null;
      shiftHeld = false;
      return;
    }
    const onKey = (e: KeyboardEvent): void => {
      shiftHeld = e.shiftKey;
    };
    const onBlur = (): void => {
      shiftHeld = false;
    };
    window.addEventListener('keydown', onKey);
    window.addEventListener('keyup', onKey);
    window.addEventListener('blur', onBlur);
    return () => {
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('keyup', onKey);
      window.removeEventListener('blur', onBlur);
    };
  });

  // Let store-side zoom writes respect the horizontal limit at the zoom they produce.
  $effect(() => {
    setScrollLimit((px: number): number => {
      const p = get(projectStore);
      const end = p ? tickToSec(timelineEndTick(p)) : 0;
      return Math.max(0, (end + SCROLL_PAD_S) * px - vpWidth);
    });
    return () => setScrollLimit(null);
  });

  // Keep the horizontal scroll inside its limit after resize, zoom or edits. Reads $scrollX so any
  // write above the limit (including when the content fits and the limit is 0) is pulled back.
  $effect(() => {
    const limit = Math.max(0, maxScrollX);
    const x = $scrollX;
    if (x > limit) scrollX.set(limit);
  });
  $effect(() => {
    const limit = maxScrollY;
    if (get(scrollY) > limit) scrollY.set(Math.max(0, limit));
  });

  // Track the viewport size.
  $effect(() => {
    const el = viewportEl;
    if (!el) return;
    const measure = (): void => {
      vpWidth = el.clientWidth;
      vpHeight = el.clientHeight;
    };
    measure();
    const observer = new ResizeObserver(measure);
    observer.observe(el);
    return () => observer.disconnect();
  });

  // Wheel: vertical scroll, Shift or a dominant horizontal delta for horizontal scroll, Ctrl for
  // zoom around the pointer. Attached with passive false so preventDefault stops page scrolling.
  // The track headers scroll the rows vertically too.
  $effect(() => {
    const el = viewportEl;
    if (!el) return;
    const onWheel = (e: WheelEvent): void => {
      e.preventDefault();
      if (e.ctrlKey) {
        const p = pointerIn(el, e);
        const factor = e.deltaY < 0 ? WHEEL_ZOOM : 1 / WHEEL_ZOOM;
        zoomBy(factor, xToTick(p.x, view));
        return;
      }
      if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) {
        scrollX.set(clampX(get(scrollX) + e.deltaX));
      } else if (e.shiftKey) {
        scrollX.set(clampX(get(scrollX) + e.deltaY));
      } else {
        scrollY.set(clampY(get(scrollY) + e.deltaY));
      }
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    return () => el.removeEventListener('wheel', onWheel);
  });

  $effect(() => {
    const el = headersEl;
    if (!el) return;
    const onWheel = (e: WheelEvent): void => {
      e.preventDefault();
      scrollY.set(clampY(get(scrollY) + e.deltaY));
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    return () => el.removeEventListener('wheel', onWheel);
  });

  // Redraw on device pixel ratio changes (a media query on the current ratio fires when it moves).
  $effect(() => {
    let media: MediaQueryList | null = null;
    const onChange = (): void => {
      dprVersion += 1;
      watchRatio();
    };
    const watchRatio = (): void => {
      media?.removeEventListener('change', onChange);
      media = window.matchMedia(`(resolution: ${window.devicePixelRatio || 1}dppx)`);
      media.addEventListener('change', onChange);
    };
    watchRatio();
    return () => media?.removeEventListener('change', onChange);
  });

  // Escape (actions.ts) cancels the active drag first and clears the selection only when none was
  // active. Registered while the timeline is mounted.
  $effect(() => {
    setDragCanceller(() => cancelActiveDrag(true));
    return () => setDragCanceller(null);
  });

  // Media-bin drags are tracked on the window while the bin reports one, so the pointer can
  // leave the bin and still show and place a drop on the timeline.
  $effect(() => {
    const bin = $binDrag;
    if (bin === null) {
      dropPreview = null;
      return;
    }
    const onMove = (e: PointerEvent): void => {
      dropPreview = binDropAt(e.clientX, e.clientY, bin.mediaId);
    };
    const onUp = (e: PointerEvent): void => {
      const target = binDropAt(e.clientX, e.clientY, bin.mediaId);
      dropPreview = null;
      if (target === null) return;
      void dispatch(placeCommand(bin.mediaId, target.tick, videoTrackFor(target.row)));
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
    return () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
    };
  });

  // Redraw the canvas when anything it shows changes. The playhead line is drawn by the DOM,
  // so playback ticks do not redraw the canvas.
  $effect(() => {
    void proj;
    void rows;
    void $scrollY;
    void $selection;
    void view;
    void frame;
    void hoverEdge;
    void selectedGap;
    void assetVersion;
    void dprVersion;
    scheduleDraw();
  });

  // Start thumbnail and waveform loading for the bin's media.
  $effect(() => {
    const current = proj;
    if (current) ensureMediaAssets(current, bumpAssets);
  });

  $effect(() => () => {
    if (rafDraw !== 0) cancelAnimationFrame(rafDraw);
    stopAutoscroll();
  });

  function bumpAssets(): void {
    assetVersion += 1;
  }

  function clampX(v: number): number {
    return Math.min(Math.max(0, v), maxScrollX);
  }

  function clampY(v: number): number {
    return Math.min(Math.max(0, v), maxScrollY);
  }

  /** Pointer position relative to an element, in timeline-area px. */
  function pointerIn(el: HTMLElement, e: MouseEvent): { x: number; y: number } {
    const r = el.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  function pointerLike(e: MouseEvent): PointerLike {
    const p = viewportEl ? pointerIn(viewportEl, e) : { x: 0, y: 0 };
    return {
      x: p.x,
      y: p.y,
      shiftKey: e.shiftKey,
      altKey: e.altKey,
      ctrlKey: e.ctrlKey,
      metaKey: e.metaKey,
    };
  }

  // ---------------------------------------------------------------------------------------------
  // Canvas
  // ---------------------------------------------------------------------------------------------

  function scheduleDraw(): void {
    if (rafDraw !== 0) return;
    rafDraw = requestAnimationFrame(() => {
      rafDraw = 0;
      drawNow();
    });
  }

  function drawNow(): void {
    const el = canvasEl;
    const current = proj;
    if (!el || !current) return;
    const ctx = el.getContext('2d');
    if (!ctx) return;
    const dpr = window.devicePixelRatio || 1;
    const pw = Math.max(1, Math.round(vpWidth * dpr));
    const ph = Math.max(1, Math.round(vpHeight * dpr));
    if (el.width !== pw) el.width = pw;
    if (el.height !== ph) el.height = ph;
    const args: DrawArgs = {
      project: current,
      rows,
      view,
      selection: get(selection),
      snapTick: frame.snapTick,
      dragGhosts: frame.ghosts,
      marquee: frame.marquee,
      ...(frame.fadePreview ? { fadePreview: frame.fadePreview } : {}),
      selectedGap,
      hoverEdge,
      devicePixelRatio: dpr,
      thumbs: getThumbStrip,
      peaks: getPeaks,
    };
    ctx.clearRect(0, 0, pw, ph);
    drawTimeline(ctx, args);
  }

  // ---------------------------------------------------------------------------------------------
  // Drag lifecycle
  // ---------------------------------------------------------------------------------------------

  /** Clip ids a drag must not snap to (they are the thing being moved). */
  function excludeIdsOf(state: DragState, current: Project): string[] {
    switch (state.kind) {
      case 'move':
        return movingClips(current, state.clipIds).map((c) => c.id);
      case 'trim-start':
      case 'trim-end':
        return movingClips(current, [state.clipId]).map((c) => c.id);
      case 'roll':
        return [state.leftId, state.rightId];
      default:
        return [];
    }
  }

  function contextFor(current: Project, state: DragState): DragContext {
    return {
      project: current,
      rows,
      view,
      snapPoints: snapTargets(current, excludeIdsOf(state, current), get(playhead)),
      snapEnabled: current.modes.snap,
    };
  }

  /** Recomputes the active drag for pointer `p` and shows the result. */
  function applyUpdate(p: PointerLike): void {
    const current = proj;
    const state = drag;
    if (!current || state === null) return;
    if (state.kind === 'pan') {
      const next = panScroll(state, p);
      scrollX.set(clampX(next.scrollX));
      scrollY.set(clampY(next.scrollY));
      return;
    }
    const update = updateDrag(state, p, contextFor(current, state));
    if (state.kind === 'marquee') selectedGap = null;
    drag = update.state;
    frame = {
      ghosts: update.ghosts,
      snapTick: update.snapTick,
      marquee: update.marquee,
      fadePreview: update.fadePreview ?? null,
    };
    if (update.previewTick !== null) seek(update.previewTick);
    if (update.selectionIds !== null) {
      selection.set(expandWithGroups(current, update.selectionIds));
    }
  }

  /**
   * Starts a drag. `immediate` drags (pan, playhead) run from the press; others wait for
   * DRAG_THRESHOLD_PX of movement so a click stays a click.
   */
  /** True while a playhead drag holds a scrub open in the store. Keeps begin/end paired. */
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

  function beginState(state: DragState | null, e: PointerEvent, p: PointerLike, immediate: boolean): void {
    if (state === null) return;
    drag = state;
    pointerId = e.pointerId;
    pressX = p.x;
    pressY = p.y;
    armed = immediate || state.kind === 'playhead';
    if (state.kind === 'playhead') {
      playheadAtPress = get(playhead);
      startScrub();
    }
    try {
      viewportEl?.setPointerCapture(e.pointerId);
    } catch {
      // The element may already have lost the pointer; the drag still works without capture.
    }
    if (armed) applyUpdate(p);
  }

  function endPointer(e: PointerEvent, cancelled: boolean): void {
    const state = drag;
    if (state === null || e.pointerId !== pointerId) return;
    try {
      if (viewportEl?.hasPointerCapture(e.pointerId)) viewportEl.releasePointerCapture(e.pointerId);
    } catch {
      // Capture was already released.
    }
    stopAutoscroll();
    stopScrub();
    drag = null;
    frame = EMPTY_FRAME;
    const wasArmed = armed;
    armed = false;
    const current = proj;

    if (!wasArmed) {
      // A plain click: a marquee clears the selection (unless Shift or Ctrl adds to it); a
      // multi-selection collapses. Blank track space never moves the playhead: only the ruler does.
      if (state.kind === 'marquee' && current) {
        if (!state.additive) selection.set([]);
      } else if (state.kind === 'move' && collapseTo !== null) {
        selectClips([collapseTo], 'replace');
      }
      collapseTo = null;
      return;
    }
    collapseTo = null;
    if (cancelled || !current) return;
    const commands: Command[] = finishDrag(state, { project: current });
    const sends = commands.map((cmd) => {
      const sent = dispatch(cmd);
      if (cmd.type === 'duplicate_clips') void sent.then(selectDuplicates);
      return sent;
    });
    // Magnetic mode keeps the tracks the clips left free of holes. Queued after the move, and only
    // when the move went through (dispatch resolves to null on failure).
    const moveIndex = commands.findIndex((cmd) => cmd.type === 'move_clips');
    const moveSent = sends[moveIndex];
    if (current.modes.magnetic && moveSent !== undefined) {
      const tracks = sourceTracksLeft(current, commands);
      void moveSent.then((reply) => {
        if (reply === null) return;
        for (const trackId of tracks) {
          void dispatch({ type: 'close_all_gaps', track_id: trackId });
        }
      });
    }
  }

  /** After a duplicate drag, the new copies become the selection. Ids the backend did not create are ignored. */
  function selectDuplicates(reply: ApplyReply | null): void {
    if (reply === null) return;
    const present = new Set(reply.snapshot.project.clips.map((c) => c.id));
    const ids = reply.applied.created.filter((id) => present.has(id));
    if (ids.length > 0) selectClips(ids, 'replace');
  }

  /** Tracks that moved clips left in a `move_clips` command (new track differs from the old). */
  function sourceTracksLeft(current: Project, commands: Command[]): string[] {
    const out: string[] = [];
    for (const cmd of commands) {
      if (cmd.type !== 'move_clips') continue;
      for (const m of cmd.moves) {
        if (m.new_track === null) continue;
        const clip = current.clips.find((c) => c.id === m.clip_id);
        if (clip && clip.track_id !== m.new_track && !out.includes(clip.track_id)) {
          out.push(clip.track_id);
        }
      }
    }
    return out;
  }

  /**
   * Drops the active drag without committing it: Esc, a new press, or lost pointer capture. With
   * `restore` (Esc), a ruler drag puts the playhead back where the press was and a marquee puts back
   * the selection it started from. Otherwise the view keeps the preview the drag already showed.
   * Returns true when a drag was active.
   */
  function cancelActiveDrag(restore = false): boolean {
    stopScrub();
    const state = drag;
    if (state === null) return false;
    try {
      if (viewportEl?.hasPointerCapture(pointerId)) viewportEl.releasePointerCapture(pointerId);
    } catch {
      // Capture was already released.
    }
    stopAutoscroll();
    if (restore) {
      if (state.kind === 'marquee') selection.set([...state.base]);
      else if (state.kind === 'playhead' && playheadAtPress !== null) seek(playheadAtPress);
    }
    drag = null;
    frame = EMPTY_FRAME;
    armed = false;
    collapseTo = null;
    playheadAtPress = null;
    return true;
  }

  function onLostCapture(e: PointerEvent): void {
    if (e.pointerId === pointerId) cancelActiveDrag();
  }

  // Auto-scroll while the pointer sits near the left or right edge during a drag.
  function autoscrollAt(p: PointerLike): void {
    lastPointer = p;
    const dir = p.x < EDGE_SCROLL_PX ? -1 : p.x > vpWidth - EDGE_SCROLL_PX ? 1 : 0;
    scrollDir = dir;
    if (dir !== 0 && rafScroll === 0) {
      rafScroll = requestAnimationFrame(autoscrollStep);
    }
  }

  function autoscrollStep(): void {
    rafScroll = 0;
    const state = drag;
    const p = lastPointer;
    if (state === null || p === null || scrollDir === 0 || state.kind === 'pan') return;
    const before = get(scrollX);
    const next = clampX(before + scrollDir * AUTOSCROLL_PX);
    const applied = next - before;
    if (applied === 0) return;
    scrollX.set(next);
    // Keep the origin in view space so the drag follows the content, not the pointer.
    drag = shiftDragOrigin(state, applied);
    applyUpdate(p);
    rafScroll = requestAnimationFrame(autoscrollStep);
  }

  function stopAutoscroll(): void {
    if (rafScroll !== 0) cancelAnimationFrame(rafScroll);
    rafScroll = 0;
    scrollDir = 0;
    lastPointer = null;
  }

  // ---------------------------------------------------------------------------------------------
  // Pointer events
  // ---------------------------------------------------------------------------------------------

  /**
   * Fade handle under the pointer, or null. Only audio clips at least FADE_MIN_CLIP_PX wide have
   * one: pointer within the top FADE_ZONE_PX of the clip and within `maxDistPx` of the fade apex.
   * With `selectedOnly`, only selected clips count (used to take a handle over a trim edge).
   */
  function fadeHandleAt(
    current: Project,
    p: PointerLike,
    maxDistPx: number,
    selectedOnly: boolean,
  ): { clipId: string; edge: 'in' | 'out' } | null {
    const selected = get(selection);
    for (const clip of current.clips) {
      if (clip.kind !== 'audio') continue;
      if (selectedOnly && !selected.includes(clip.id)) continue;
      const r = clipRect(clip, rows, view);
      if (!r || r.w < FADE_MIN_CLIP_PX) continue;
      if (p.y < r.y || p.y > r.y + FADE_ZONE_PX) continue;
      const inX = tickToX(clip.start + clip.audio.fade_in, view);
      const outX = tickToX(clipEnd(clip) - clip.audio.fade_out, view);
      // With selectedOnly (the take-over of a trim edge), a zero-length fade only counts when the
      // press is clearly below the clip top, so an edge trim from the top corner stays a trim.
      const deep = p.y - r.y >= FADE_EDGE_MIN_DEPTH_PX;
      if (Math.abs(p.x - inX) <= maxDistPx && (!selectedOnly || clip.audio.fade_in > 0 || deep)) {
        return { clipId: clip.id, edge: 'in' };
      }
      if (Math.abs(p.x - outX) <= maxDistPx && (!selectedOnly || clip.audio.fade_out > 0 || deep)) {
        return { clipId: clip.id, edge: 'out' };
      }
    }
    return null;
  }

  /** True when an event starts on the ruler, which Ruler.svelte handles (seek, work area, markers). */
  function isRulerTarget(e: Event): boolean {
    return e.target instanceof Element && e.target.closest('.ruler-wrap') !== null;
  }

  /** Applies the press modifiers to the selection before a clip drag. Returns false to cancel. */
  function pressClipSelection(clipId: string, e: PointerEvent): boolean {
    collapseTo = null;
    if (e.ctrlKey || e.metaKey) {
      selectClips([clipId], 'toggle');
      return get(selection).includes(clipId);
    }
    if (e.shiftKey) {
      selectClips([clipId], 'add');
      return true;
    }
    const current = get(selection);
    if (!current.includes(clipId)) {
      selectClips([clipId], 'replace');
    } else if (current.length > 1) {
      collapseTo = clipId;
    }
    return true;
  }

  function onPointerDown(e: PointerEvent): void {
    // Any press replaces the gap selection; a press on a gap selects that gap again below.
    selectedGap = null;
    if (isRulerTarget(e)) return;
    const current = proj;
    if (!current || isBusy()) return;
    cancelActiveDrag();
    const p = pointerLike(e);

    if (e.button === 1 || ($tool === 'hand' && e.button === 0)) {
      beginState(beginPan(p, get(scrollX), get(scrollY)), e, p, true);
      return;
    }
    if (e.button !== 0) return;

    const hit: HitResult = hitTest(current, rows, view, p.x, p.y);
    const tickAt = hit.tick ?? xToTick(p.x, view);

    if ($tool === 'zoom') {
      zoomBy(e.altKey ? 1 / 2 : 2, tickAt);
      return;
    }
    if ($tool === 'razor') {
      if (hit.kind === 'clip' && hit.clipId) {
        // Same snapped tick as the cut preview; Shift on this press disables snapping.
        const cut = cutSnapPoint(current, p.x, e.shiftKey, get(playhead), view, SNAP_PX);
        void dispatch({ type: 'split_at', time: cut.tick, ids: [hit.clipId] });
      }
      return;
    }

    // Fade handles and envelope points come before the clip body. Trim edges win over them, except
    // a fade handle on a selected audio clip that is close to the press (see FADE_EDGE_PREF_PX).
    const onEdge = hit.kind === 'edge-start' || hit.kind === 'edge-end';
    const fade = fadeHandleAt(current, p, onEdge ? FADE_EDGE_PREF_PX : FADE_HIT_PX, onEdge);
    if (fade) {
      beginState(beginFadeDrag(current, fade.clipId, fade.edge, p), e, p, false);
      return;
    }
    if (!onEdge) {
      // The envelope's gain line counts only on selected clips; an envelope point always counts.
      const env = hitEnvelopePoint(current, rows, view, p.x, p.y, get(selection));
      if (env !== null) {
        // A point drags that point; the gain line inserts a point at the press and drags it.
        const envState =
          env.index !== null
            ? beginEnvelopeDrag(current, env.clipId, env.index, p)
            : beginEnvelopeHitDrag(current, env, p);
        beginState(envState, e, p, false);
        return;
      }
    }
    if (hit.kind === 'clip' && hit.clipId) {
      if (!pressClipSelection(hit.clipId, e)) return;
    } else if ((hit.kind === 'edge-start' || hit.kind === 'edge-end') && hit.clipId) {
      if (!get(selection).includes(hit.clipId)) selectClips([hit.clipId], 'replace');
    } else if (hit.kind === 'gap') {
      selectGap(hit, e);
    }
    const state = beginDrag(hit, p, current, get(selection));
    beginState(state, e, p, state?.kind === 'playhead');
  }

  /** Makes a gap hit the selection: clears the clips unless a modifier adds to them. */
  function selectGap(hit: HitResult, e: MouseEvent): void {
    const gap = gapOf(hit);
    if (gap === null) return;
    if (!e.shiftKey && !e.ctrlKey && !e.metaKey) selection.set([]);
    selectedGap = gap;
  }

  /** Gap span of a `gap` hit, or null when the hit lacks one. */
  function gapOf(hit: HitResult): { trackId: string; start: Tick; end: Tick } | null {
    if (hit.kind !== 'gap' || !hit.trackId || hit.gapStart === undefined || hit.gapEnd === undefined) {
      return null;
    }
    return { trackId: hit.trackId, start: hit.gapStart, end: hit.gapEnd };
  }

  /**
   * Closes a gap: the clips after it on its track move left. `at` is the gap start, which the
   * backend accepts as a time inside the gap. Clears the gap selection.
   */
  function deleteGap(gap: { trackId: string; start: Tick }): void {
    selectedGap = null;
    void dispatch({ type: 'close_gap', track_id: gap.trackId, at: gap.start });
  }

  /** Delete and Backspace act on a selected gap here, so the global shortcut does not run. */
  function onViewportKey(e: KeyboardEvent): void {
    if (selectedGap === null) return;
    if (e.key === 'Delete' || e.key === 'Backspace') {
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      e.preventDefault();
      e.stopPropagation();
      deleteGap(selectedGap);
      return;
    }
    if (e.key === 'Escape') selectedGap = null;
  }

  /**
   * Idle hover: remembers the edge or gap under the pointer (select tool only) so the cursor and
   * the edge handle can show what a press will do. Assigns only when the hit changes, so a still
   * pointer does not redraw.
   */
  function updateHover(p: PointerLike): void {
    const current = proj;
    let next: HitResult | null = null;
    if (current && $tool === 'select') {
      const hit = hitTest(current, rows, view, p.x, p.y);
      if (hit.kind === 'edge-start' || hit.kind === 'edge-end' || hit.kind === 'gap') next = hit;
    }
    if (hoverKey(next) !== hoverKey(hover)) hover = next;
  }

  function hoverKey(h: HitResult | null): string {
    if (h === null) return '';
    return [h.kind, h.clipId ?? '', h.trackId ?? '', h.gapStart ?? '', h.gapEnd ?? ''].join('|');
  }

  function onPointerMove(e: PointerEvent): void {
    const state = drag;
    if (state === null) {
      const p = pointerLike(e);
      updateHover(p);
      if ($tool === 'razor') {
        cutPos = { x: p.x, y: p.y };
        shiftHeld = e.shiftKey;
      }
      return;
    }
    if (e.pointerId !== pointerId) return;
    const p = pointerLike(e);
    if (!armed) {
      if (Math.hypot(p.x - pressX, p.y - pressY) < DRAG_THRESHOLD_PX) return;
      armed = true;
    }
    if (state.kind !== 'pan') autoscrollAt(p);
    applyUpdate(p);
  }

  function onPointerUp(e: PointerEvent): void {
    endPointer(e, false);
  }

  function onPointerCancel(e: PointerEvent): void {
    endPointer(e, true);
  }

  /** Double-click on an envelope line adds a point; on a point it removes that point. */
  function envelopeDoubleClick(current: Project, p: PointerLike): boolean {
    const env = hitEnvelopePoint(current, rows, view, p.x, p.y);
    if (env === null) return false;
    const clipId = env.clipId;
    if (env.index !== null) {
      const index = env.index;
      void dispatchWith((latest) => {
        const clip = latest.clips.find((c) => c.id === clipId);
        if (!clip || index >= clip.audio.envelope.length) return null;
        return {
          type: 'set_envelope',
          clip_id: clipId,
          points: clip.audio.envelope.filter((_, i) => i !== index),
        };
      });
      return true;
    }
    const clip = current.clips.find((c) => c.id === clipId);
    if (!clip) return false;
    const t = clampTo(xToTick(p.x, view) - clip.start, 0, clip.dur);
    const db = env.db;
    void dispatchWith((latest) => {
      const target = latest.clips.find((c) => c.id === clipId);
      if (!target) return null;
      const points = [...target.audio.envelope, { t, db }].sort((a, b) => a.t - b.t);
      return { type: 'set_envelope', clip_id: clipId, points };
    });
    return true;
  }

  function onDoubleClick(e: MouseEvent): void {
    if (isRulerTarget(e)) return;
    const current = proj;
    if (!current) return;
    const p = pointerLike(e);
    if (envelopeDoubleClick(current, p)) return;
    const hit = hitTest(current, rows, view, p.x, p.y);
    // Enter the group: select only this clip, without expanding its group.
    if (hit.kind === 'clip' && hit.clipId) selection.set([hit.clipId]);
    // Double-clicking a gap closes it.
    const gap = gapOf(hit);
    if (gap !== null) deleteGap(gap);
  }

  /** Clamps `v` into `lo..hi`. */
  function clampTo(v: number, lo: number, hi: number): number {
    return Math.min(hi, Math.max(lo, v));
  }

  // ---------------------------------------------------------------------------------------------
  // Context menu
  // ---------------------------------------------------------------------------------------------

  const SEPARATOR: MenuItem = { label: '', separator: true };

  function openMenu(e: MouseEvent, items: MenuItem[]): void {
    if (items.length === 0) return;
    menu = { x: e.clientX, y: e.clientY, items };
  }

  function closeMenu(): void {
    menu = null;
  }

  function clipMenu(current: Project, clipId: string): MenuItem[] {
    const ids = get(selection);
    const clips = current.clips.filter((c) => ids.includes(c.id));
    const clicked = current.clips.find((c) => c.id === clipId);
    const play = get(playhead);
    const hasVideo = clips.some((c) => c.kind === 'video');
    const inside = clicked !== undefined && play > clicked.start && play < clipEnd(clicked);
    // Mute applies to audio clips only: the selection's audio clips plus their audio link partners.
    const linkIds = new Set(
      clips.map((c) => c.link_id).filter((l): l is string => l !== null),
    );
    const muteIds = current.clips
      .filter((c) => c.kind === 'audio' && (ids.includes(c.id) || (c.link_id !== null && linkIds.has(c.link_id))))
      .map((c) => c.id);
    // Only offer what would change something.
    const sharedOne = (vals: (string | null)[]): boolean =>
      vals.length > 0 && vals[0] !== null && vals.every((v) => v === vals[0]);
    const canGroup = clips.length >= 2 && !sharedOne(clips.map((c) => c.group_id));
    const canUngroup = clips.some((c) => c.group_id !== null);
    const canLink = clips.length >= 2 && !sharedOne(clips.map((c) => c.link_id));
    const canUnlink = clips.some((c) => c.link_id !== null);
    const muteClips = current.clips.filter((c) => muteIds.includes(c.id));
    const canMute = muteClips.some((c) => !c.audio.muted);
    const canUnmute = muteClips.some((c) => c.audio.muted);
    const canEnable = clips.some((c) => !c.enabled);
    const canDisable = clips.some((c) => c.enabled);
    // Edits are off when any selected clip sits on a locked track (or on no known track).
    const lockedClip = (c: Clip): boolean => current.tracks.find((t) => t.id === c.track_id)?.locked ?? true;
    const canEdit = clips.length > 0 && !clips.some(lockedClip);
    // Split needs the playhead strictly inside a selected clip; speed needs a media (video or audio) clip.
    const canSplit = canEdit && clips.some((c) => play > c.start && play < clipEnd(c));
    const canSpeed = canEdit && clips.some((c) => c.kind === 'video' || c.kind === 'audio');
    return [
      {
        label: 'Split at playhead',
        shortcut: 'Ctrl+K',
        disabled: !canSplit,
        action: () => {
          void dispatch({ type: 'split_at', time: play, ids });
        },
      },
      SEPARATOR,
      {
        label: 'Cut',
        shortcut: 'Ctrl+X',
        disabled: !canEdit,
        action: () => {
          // Delete only when the copy went through.
          void copySelection().then((reply) => {
            if (reply !== null) void dispatch({ type: 'delete_clips', ids, ripple: false });
          });
        },
      },
      {
        label: 'Copy',
        shortcut: 'Ctrl+C',
        action: () => {
          void copySelection();
        },
      },
      {
        label: 'Paste at playhead',
        shortcut: 'Ctrl+V',
        action: () => {
          void pasteAtPlayhead();
        },
      },
      {
        label: 'Duplicate',
        shortcut: 'Ctrl+D',
        disabled: !canEdit,
        action: () => {
          // The offset is measured on the latest project, not the one the menu was opened on.
          void dispatchWith((latest) => {
            const picked = latest.clips.filter((c) => ids.includes(c.id));
            if (picked.length === 0) return null;
            const span =
              Math.max(...picked.map((c) => clipEnd(c))) - Math.min(...picked.map((c) => c.start));
            return { type: 'duplicate_clips', ids, offset: span };
          });
        },
      },
      SEPARATOR,
      {
        label: 'Delete',
        shortcut: 'Delete',
        disabled: !canEdit,
        action: () => {
          void dispatch({ type: 'delete_clips', ids, ripple: false });
        },
      },
      {
        label: 'Ripple delete',
        shortcut: 'Shift+Delete',
        disabled: !canEdit,
        action: () => {
          void dispatch({ type: 'delete_clips', ids, ripple: true });
        },
      },
      SEPARATOR,
      {
        label: 'Group',
        shortcut: 'Ctrl+G',
        disabled: !canEdit || !canGroup,
        action: () => {
          void dispatch({ type: 'group_clips', ids });
        },
      },
      {
        label: 'Ungroup',
        shortcut: 'Ctrl+Shift+G',
        disabled: !canEdit || !canUngroup,
        action: () => {
          void dispatch({ type: 'ungroup', ids });
        },
      },
      {
        label: 'Link audio and video',
        disabled: !canEdit || !canLink,
        action: () => {
          void dispatch({ type: 'link_clips', ids });
        },
      },
      {
        label: 'Unlink audio and video',
        disabled: !canEdit || !canUnlink,
        action: () => {
          void dispatch({ type: 'unlink_clips', ids });
        },
      },
      SEPARATOR,
      {
        label: 'Detach audio',
        disabled: !canEdit || !hasVideo,
        action: () => {
          for (const c of clips) {
            if (c.kind === 'video') void dispatch({ type: 'detach_audio', clip_id: c.id });
          }
        },
      },
      {
        label: 'Mute clip',
        disabled: !canEdit || !canMute,
        action: () => {
          void dispatch({ type: 'patch_clips', ids: muteIds, patch: { muted: true } });
        },
      },
      {
        label: 'Unmute clip',
        disabled: !canEdit || !canUnmute,
        action: () => {
          void dispatch({ type: 'patch_clips', ids: muteIds, patch: { muted: false } });
        },
      },
      {
        label: 'Enable clip',
        disabled: !canEdit || !canEnable,
        action: () => {
          void dispatch({ type: 'patch_clips', ids, patch: { enabled: true } });
        },
      },
      {
        label: 'Disable clip',
        disabled: !canEdit || !canDisable,
        action: () => {
          void dispatch({ type: 'patch_clips', ids, patch: { enabled: false } });
        },
      },
      SEPARATOR,
      ...[0.5, 1, 2].map((speed) => ({
        label: `Speed ${speed}x`,
        disabled: !canSpeed,
        action: () => {
          for (const id of ids) {
            void dispatch({ type: 'set_speed', clip_id: id, speed, ripple: true });
          }
        },
      })),
      SEPARATOR,
      {
        label: 'Freeze frame at playhead',
        disabled: !canEdit || !(clicked?.kind === 'video' && inside),
        action: () => {
          void dispatch({
            type: 'freeze_frame',
            clip_id: clipId,
            at: play,
            dur: secToTick(FREEZE_S),
          });
        },
      },
    ];
  }

  /** Menu for a right-clicked gap: one item, which closes that gap. */
  function gapMenu(gap: { trackId: string; start: Tick }): MenuItem[] {
    return [
      {
        label: 'Delete gap',
        shortcut: 'Delete',
        action: () => {
          deleteGap(gap);
        },
      },
    ];
  }

  function rulerMenu(hit: HitResult, current: Project): MenuItem[] {
    const t = hit.tick ?? 0;
    const area = current.work_area;
    // In and out stay inside the timeline. An empty timeline has no in point to set.
    const timelineEnd = timelineEndTick(current);
    const hasTimeline = timelineEnd > 0;
    const at = Math.min(t, timelineEnd);
    const items: MenuItem[] = [
      {
        label: 'Add marker here',
        shortcut: 'M',
        action: () => {
          void dispatch({ type: 'add_marker', time: t, label: 'Marker', color: null });
        },
      },
      SEPARATOR,
      {
        label: 'Set in point here',
        shortcut: 'I',
        disabled: !hasTimeline || t >= timelineEnd,
        action: () => {
          const end = area !== null && area.end > t ? Math.min(area.end, timelineEnd) : timelineEnd;
          void dispatch({ type: 'set_work_area', area: { start: t, end } });
        },
      },
      {
        label: 'Set out point here',
        shortcut: 'O',
        disabled: at <= 0,
        action: () => {
          const start = area !== null && area.start < at ? area.start : 0;
          void dispatch({ type: 'set_work_area', area: { start, end: at } });
        },
      },
      {
        label: 'Clear in and out',
        shortcut: 'Alt+X',
        action: () => {
          void dispatch({ type: 'set_work_area', area: null });
        },
      },
    ];
    if (area !== null) {
      const range = { start: area.start, end: area.end };
      items.push(
        SEPARATOR,
        {
          label: 'Extract range',
          action: () => {
            void dispatch({ type: 'remove_range', ...range, track_ids: [], ripple: true });
          },
        },
        {
          label: 'Lift range',
          action: () => {
            void dispatch({ type: 'remove_range', ...range, track_ids: [], ripple: false });
          },
        },
      );
    }
    items.push(
      SEPARATOR,
      {
        label: 'Zoom to fit',
        shortcut: '\\',
        action: () => {
          zoomToFit(vpWidth);
        },
      },
    );
    return items;
  }

  function onContextMenu(e: MouseEvent): void {
    e.preventDefault();
    const current = proj;
    if (!current) return;
    const p = pointerLike(e);
    const hit = hitTest(current, rows, view, p.x, p.y);
    const gap = gapOf(hit);

    // Within a few px of a clip edge still counts as the clip.
    if ((hit.kind === 'clip' || hit.kind === 'edge-start' || hit.kind === 'edge-end') && hit.clipId) {
      if (!get(selection).includes(hit.clipId)) selectClips([hit.clipId], 'replace');
      openMenu(e, clipMenu(current, hit.clipId));
    } else if (gap !== null) {
      selection.set([]);
      selectedGap = gap;
      openMenu(e, gapMenu(gap));
    } else if (hit.kind === 'ruler') {
      openMenu(e, rulerMenu(hit, current));
    } else if (hit.kind === 'track' && hit.trackId) {
      openMenu(e, [
        { label: 'Paste at playhead', action: () => void pasteAtPlayhead() },
        { label: 'Select all', action: selectAllClips },
        SEPARATOR,
        { label: 'Zoom to fit', action: () => zoomToFit(vpWidth) },
      ]);
    } else {
      openMenu(e, [
        { label: 'Select all', action: selectAllClips },
        SEPARATOR,
        { label: 'Zoom to fit', action: () => zoomToFit(vpWidth) },
      ]);
    }
  }

  // ---------------------------------------------------------------------------------------------
  // Drag and drop from the media bin
  // ---------------------------------------------------------------------------------------------

  /** A drop target: snapped tick, and the row under the pointer when it is a row of the right kind. */
  interface DropPreview {
    tick: Tick;
    row: TrackRow | null;
  }

  function hasDropType(e: DragEvent): boolean {
    return Array.from(e.dataTransfer?.types ?? []).includes(RECORDING_MIME);
  }

  /**
   * Drop target at a client position, or null when the position is outside the timeline area.
   * `kind` is the track kind the dropped item goes to; rows of another kind are not targets.
   */
  function dropAt(clientX: number, clientY: number, kind: TrackKind): DropPreview | null {
    const current = proj;
    const el = viewportEl;
    if (!current || !el) return null;
    const r = el.getBoundingClientRect();
    const x = clientX - r.left;
    const y = clientY - r.top;
    if (x < 0 || y < 0 || x > vpWidth || y > vpHeight) return null;
    const raw = xToTick(x, view);
    const tick = current.modes.snap
      ? snapTick(raw, snapTargets(current, [], get(playhead)), SNAP_PX, view.pxPerSec).tick
      : raw;
    const hit = hitTest(current, rows, view, x, y);
    const row = hit.trackId ? (rows.find((t) => t.trackId === hit.trackId) ?? null) : null;
    return { tick, row: row !== null && row.kind === kind ? row : null };
  }

  /** Track kind a bin item drops onto: audio media goes to audio rows, video and images to video rows. */
  function mediaTrackKind(current: Project, mediaId: string): TrackKind {
    const media = current.media.find((m) => m.id === mediaId);
    return media?.kind === 'audio' ? 'audio' : 'video';
  }

  /** Drop target for a media-bin item at a client position, or null outside the timeline. */
  function binDropAt(clientX: number, clientY: number, mediaId: string): DropPreview | null {
    const current = proj;
    if (!current) return null;
    return dropAt(clientX, clientY, mediaTrackKind(current, mediaId));
  }

  /** Video track id for a placement: only video rows take the clip as a video track. */
  function videoTrackFor(row: TrackRow | null): string | null {
    return row?.kind === 'video' ? row.trackId : null;
  }

  function onDragOver(e: DragEvent): void {
    if (!hasDropType(e)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'copy';
    dropPreview = dropAt(e.clientX, e.clientY, 'video');
  }

  function onDragLeave(e: DragEvent): void {
    const related = e.relatedTarget;
    if (!(related instanceof Node) || !viewportEl?.contains(related)) dropPreview = null;
  }

  /** Reads one string field from a JSON drag payload, or null. */
  function payloadField(raw: string, key: string): string | null {
    if (raw === '') return null;
    try {
      const value: unknown = JSON.parse(raw);
      if (typeof value === 'object' && value !== null && key in value) {
        const field = (value as Record<string, unknown>)[key];
        return typeof field === 'string' && field !== '' ? field : null;
      }
    } catch {
      return null;
    }
    return null;
  }

  function placeCommand(mediaId: string, at: Tick, videoTrack: string | null): Command {
    return {
      type: 'place_media',
      media_id: mediaId,
      at,
      src_in: 0,
      src_len: 0,
      video_track: videoTrack,
      mode: 'overwrite',
    };
  }

  async function onDrop(e: DragEvent): Promise<void> {
    if (!hasDropType(e)) return;
    e.preventDefault();
    const target = dropAt(e.clientX, e.clientY, 'video');
    dropPreview = null;
    const dt = e.dataTransfer;
    if (!target || !dt) return;
    const recordingId = payloadField(dt.getData(RECORDING_MIME), 'recordingId');
    if (recordingId === null) return;
    try {
      const media: MediaRef = await editorApi.mediaFromRecording(recordingId);
      const reply = await dispatch({ type: 'add_media', media });
      if (reply === null) return;
      // The backend may answer with an existing media id for the same path.
      const mediaId = reply.snapshot.project.media.find((m) => m.path === media.path)?.id ?? media.id;
      void dispatch(placeCommand(mediaId, target.tick, videoTrackFor(target.row)));
    } catch (err) {
      toast.set(String(err));
    }
  }

  // ---------------------------------------------------------------------------------------------
  // Template helpers
  // ---------------------------------------------------------------------------------------------

  /** Row of the drop preview, for its highlight. */
  const dropRow = $derived(dropPreview?.row ?? null);
  const dropX = $derived(dropPreview ? tickToX(dropPreview.tick, view) : 0);
</script>

<div class="shell">
<div class="timeline">
  <div class="headers" aria-label="Track headers" bind:this={headersEl}>
    <div class="corner"></div>
    {#each rows as row (row.trackId)}
      {@const track = trackById.get(row.trackId)}
      {#if track}
        <div class="header-slot" style:top="{row.top}px" style:height="{row.height}px">
          <TrackHeader {track} height={row.height} />
        </div>
      {/if}
    {/each}
  </div>

  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
    class="viewport {cursorClass}"
    bind:this={viewportEl}
    role="application"
    tabindex="0"
    aria-label="Timeline. Drag clips to move, drag edges to trim, click the ruler to seek."
    onpointerdown={onPointerDown}
    onpointermove={onPointerMove}
    onpointerup={onPointerUp}
    onpointercancel={onPointerCancel}
    onlostpointercapture={onLostCapture}
    onpointerleave={() => {
      if (drag === null) hover = null;
      cutPos = null;
    }}
    onkeydown={onViewportKey}
    onblur={() => {
      // Keys go elsewhere once focus leaves the viewport, so the gap is no longer selected.
      selectedGap = null;
    }}
    oncontextmenu={onContextMenu}
    ondblclick={onDoubleClick}
    ondragover={onDragOver}
    ondragleave={onDragLeave}
    ondrop={onDrop}
  >
    <canvas bind:this={canvasEl} aria-hidden="true"></canvas>

    {#if proj}
      <div class="ruler-wrap">
        <Ruler {view} project={proj} onseek={seek} />
      </div>

      {#if playVisible}
        <div class="playhead" style:left="{playX}px" aria-hidden="true"></div>
      {/if}

      {#if cutPreview}
        <div
          class="cut-line"
          class:snapped={cutPreview.snapped}
          style:left="{cutPreview.x}px"
          aria-hidden="true"
        ></div>
        {#if cutPreview.snapped}
          <div class="cut-marker" class:playhead-snap={cutPreview.kind === 'playhead'} style:left="{cutPreview.x}px" aria-hidden="true"></div>
        {/if}
        <div class="cut-label" style:left="{cutPreview.x + 4}px" aria-hidden="true">{cutPreview.label}</div>
      {/if}

      {#if dropPreview}
        <div class="drop-line" style:left="{dropX}px" aria-hidden="true"></div>
        {#if dropRow}
          <div
            class="drop-row"
            style:top="{dropRow.top}px"
            style:height="{dropRow.height}px"
            aria-hidden="true"
          ></div>
        {/if}
      {/if}
    {/if}
  </div>
</div>

<div class="hbar">
  <div class="zoombox">
    <button type="button" class="zbtn" aria-label="Zoom out" title="Zoom out" onclick={() => zoomBy(0.8, xToTick(vpWidth / 2, view))}>&minus;</button>
    <input
      class="zslider"
      type="range"
      min="0"
      max="1000"
      step="1"
      aria-label="Timeline zoom"
      title="Timeline zoom (Ctrl + wheel also zooms)"
      value={zoomToSlider($pxPerSec)}
      oninput={onZoomSlider}
    />
    <button type="button" class="zbtn" aria-label="Zoom in" title="Zoom in" onclick={() => zoomBy(1.25, xToTick(vpWidth / 2, view))}>+</button>
  </div>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="scrolltrack" bind:this={trackEl} onpointerdown={onTrackDown} role="scrollbar" aria-controls="" aria-orientation="horizontal" aria-valuemin="0" aria-valuemax={Math.round(maxScrollX)} aria-valuenow={Math.round($scrollX)} tabindex="-1">
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="thumb"
      class:inactive={maxScrollX <= 0}
      style:left="{thumbLeft}px"
      style:width="{thumbWidth}px"
      onpointerdown={onThumbDown}
      onpointermove={onThumbMove}
      onpointerup={onThumbUp}
      onpointercancel={onThumbUp}
    ></div>
  </div>
</div>
</div>

{#if menu}
  <ContextMenu x={menu.x} y={menu.y} items={menu.items} onclose={closeMenu} />
{/if}

<style>
  .shell {
    display: flex;
    flex-direction: column;
    width: 100%;
    height: 100%;
    min-height: 0;
    background: var(--bg);
  }

  .hbar {
    flex: 0 0 18px;
    display: flex;
    align-items: center;
    border-top: 1px solid var(--border);
    background: var(--surface);
  }

  .zoombox {
    flex: 0 0 140px;
    width: 140px;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 6px;
    box-sizing: border-box;
  }

  .zbtn {
    flex: 0 0 auto;
    width: 16px;
    height: 16px;
    padding: 0;
    line-height: 14px;
    border: 1px solid var(--border);
    border-radius: 3px;
    background: var(--surface-2);
    color: var(--text);
    cursor: pointer;
  }

  .zslider {
    flex: 1 1 auto;
    min-width: 0;
    height: 12px;
    margin: 0;
  }

  .scrolltrack {
    position: relative;
    flex: 1 1 auto;
    min-width: 0;
    height: 12px;
    margin: 0 6px;
    border-radius: 6px;
    background: var(--surface-2);
    touch-action: none;
  }

  .thumb {
    position: absolute;
    top: 1px;
    height: 10px;
    border-radius: 5px;
    background: var(--text-dim, #8b93a1);
    opacity: 0.7;
    cursor: pointer;
  }

  .thumb:hover {
    opacity: 1;
  }

  .thumb.inactive {
    opacity: 0.25;
    cursor: default;
  }

  .timeline {
    position: relative;
    display: flex;
    width: 100%;
    flex: 1 1 auto;
    min-height: 0;
    background: var(--bg);
    color: var(--text);
    overflow: hidden;
  }

  .headers {
    position: relative;
    flex: 0 0 140px;
    width: 140px;
    height: 100%;
    overflow: hidden;
    background: var(--surface);
    border-right: 1px solid var(--border);
  }

  .corner {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    height: 28px;
    z-index: 2;
    background: var(--surface-2);
    border-bottom: 1px solid var(--border);
  }

  .header-slot {
    position: absolute;
    left: 0;
    right: 0;
  }

  .viewport {
    position: relative;
    flex: 1 1 auto;
    min-width: 0;
    height: 100%;
    overflow: hidden;
    touch-action: none;
  }

  .viewport.pan {
    cursor: grab;
  }

  .viewport.moving {
    cursor: grabbing;
  }

  .viewport.cut {
    cursor: crosshair;
  }

  .viewport.zoom {
    cursor: zoom-in;
  }

  .viewport.trim {
    cursor: ew-resize;
  }

  .viewport.roll {
    cursor: col-resize;
  }

  .viewport.gap {
    cursor: pointer;
  }

  canvas {
    position: absolute;
    left: 0;
    top: 0;
    width: 100%;
    height: 100%;
    display: block;
  }

  .ruler-wrap {
    position: absolute;
    left: 0;
    right: 0;
    top: 0;
    height: 28px;
    z-index: 1;
    pointer-events: auto;
  }

  .playhead {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 0;
    border-left: 1px solid var(--accent);
    pointer-events: none;
    z-index: 3;
  }

  .playhead::before {
    content: '';
    position: absolute;
    top: 0;
    left: -6px;
    border-left: 6px solid transparent;
    border-right: 6px solid transparent;
    border-top: 9px solid var(--accent);
  }

  /* Cut preview (Cut tool): dashed yellow line over the track rows, solid when snapped. */
  .cut-line {
    position: absolute;
    top: 28px;
    bottom: 0;
    width: 0;
    border-left: 1px dashed #ffe066;
    pointer-events: none;
    z-index: 2;
  }

  .cut-line.snapped {
    border-left: 2px solid #ffe066;
  }

  .cut-marker {
    position: absolute;
    top: 22px;
    width: 8px;
    height: 8px;
    margin-left: -4px;
    background: #ffe066;
    transform: rotate(45deg);
    pointer-events: none;
    z-index: 2;
  }

  .cut-marker.playhead-snap {
    background: #ffffff;
  }

  .cut-label {
    position: absolute;
    top: 30px;
    padding: 1px 4px;
    border-radius: 3px;
    background: rgba(20, 20, 20, 0.85);
    color: #ffe066;
    font-size: 11px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    pointer-events: none;
    z-index: 2;
  }

  .drop-line {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 0;
    border-left: 2px solid var(--accent);
    pointer-events: none;
    z-index: 4;
  }

  .drop-row {
    position: absolute;
    left: 0;
    right: 0;
    border: 1px dashed var(--accent);
    background: color-mix(in srgb, var(--accent) 10%, transparent);
    pointer-events: none;
    z-index: 3;
  }
</style>
