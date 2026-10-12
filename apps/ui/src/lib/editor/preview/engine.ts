// Program monitor engine. Plays a RenderPlan in the webview: a clock drives a pool of <video>
// elements (one per media id) for pictures and sound, a Web Audio graph (audioGraph.ts) applies
// the plan's gain curves, and compose.ts paints each frame onto the attached canvas.
//
// Timing model: the clock (clock.ts) is the single source of timeline time. Every frame, each
// needed element is checked against the source time that the scheduler derives for it. A playing
// element that drifts by 0.04 s to 0.5 s is nudged by 5 % of its rate; beyond 0.5 s it is re-seeked,
// with the measured cost of its last seek added as lead. Re-seeks back off up to 2 s apart.
// Elements of frozen (speed 0), reversed layers, or reverse shuttle cannot play backwards, so they
// are paused and re-seeked at about 12 Hz.
//
// Limits of this version:
// - Reverse shuttle, freeze and reversed layers show stepped frames (about 12 Hz). Their audio is
//   silent.
// - Rates: forward 1/16 to 8, reverse -1/16 to -8. A zero or non-finite rate acts as 1.
// - One element per media id: when two timeline sources of the same media need different source
//   times in one segment, the first (video layer, then audio source) wins and the other source
//   plays the wrong part. Linked audio and video of one clip always agree.
// - Only the default audio stream of a file is audible (see audioGraph.ts). Other audio tracks are
//   skipped, with one message per media and track.
// - More than MAX_ELEMENTS media needed at once exceed the pool cap rather than stop playback.
// - While the window is hidden the picture stops (rAF); the clock keeps running on a 100 ms timer
//   that updates audio, time and levels without painting.

import type { AudioSource, PlanSegment, Project, RenderPlan, Rgba, Tick, Transform } from '../types';
import { secToTick, tickToSec } from '../ticks';
import { createClock } from './clock';
import { AudioGraph } from './audioGraph';
import { gainRamp, segmentAt, sourceTimeFor, upcoming, type GainPoint } from './scheduler';
import { composeFrame, type SourceFrames } from './compose';

/** Preview resolution as a fraction of the project canvas. */
export type PreviewQuality = 'full' | 'half' | 'quarter';

/** Public surface of the program monitor. */
export interface PreviewEngine {
  /** Binds the canvas that receives frames. Replaces a previous canvas. */
  attach(canvas: HTMLCanvasElement): void;
  /** Loads a new plan. Keeps the playhead (clamped to the plan) and redraws. */
  setProject(project: Project, plan: RenderPlan): void;
  /** Moves the playhead. Playback continues from the new position when playing. */
  seek(t: Tick): void;
  /**
   * Starts playback from the playhead. Call from a user gesture. Negative rates play in reverse
   * (stepped frames, silent audio) and stop at the start of the plan. Zero acts as 1.
   */
  play(rate?: number): void;
  /** Stops playback at the playhead and mutes the sources. */
  pause(): void;
  /** True while the clock runs. */
  isPlaying(): boolean;
  /** Sets the master fader (linear, 1 = unity). */
  setMasterVolume(linear: number): void;
  /** Sets the preview resolution and resizes the canvas. */
  setQuality(q: 'full' | 'half' | 'quarter'): void;
  /** Subscribes to playhead updates. Returns the unsubscribe function. */
  onTime(cb: (t: Tick) => void): () => void;
  /** Subscribes to meter readings (about 10 Hz while playing). Returns the unsubscribe function. */
  onLevels(cb: (l: { master: number; tracks: number[] }) => void): () => void;
  /** Subscribes to errors. Each media reports its failure once. Returns the unsubscribe function. */
  onError(cb: (message: string) => void): () => void;
  /** Subscribes to playback reaching the end of the plan. Returns the unsubscribe function. */
  onEnded(cb: () => void): () => void;
  /**
   * Subscribes to the share of frames in the last second that were dropped (a missing source frame
   * or a step over 20 ms). Fired about once per second while playing. Returns the unsubscribe function.
   */
  onDroppedFrames(cb: (ratio: number) => void): () => void;
  /**
   * Paints `transform` for the clip `clipId` instead of its plan transform, or removes the override
   * with `null`. Affects painting only; the plan is not changed.
   */
  setTransformOverride(clipId: string, transform: Transform | null): void;
  /** Pauses, releases elements and the audio graph, and cancels all timers. */
  destroy(): void;
}

/** Options for {@link createPreviewEngine}. */
export interface PreviewEngineOptions {
  /** Resolves a playable URL for a media id. Rejections mark the media unavailable. */
  mediaUrl: (mediaId: string) => Promise<string>;
}

/** Maximum number of pooled video elements (media that are not needed are evicted first). */
const MAX_ELEMENTS = 4;
/** Playback rate magnitude bounds (the clock limits the timeline rate to 8 as well). */
const MIN_RATE = 0.0625;
const MAX_RATE = 8;
/** Drift (seconds) above which a playing element is re-seeked. Smaller drift is nudged. */
const DRIFT_SEEK_S = 0.5;
/** Drift (seconds) above which a playing element is nudged. Smaller drift runs at nominal rate. */
const DRIFT_NUDGE_S = 0.04;
/** Relative rate change applied while nudging a playing element. */
const NUDGE_FACTOR = 0.05;
/** Drift (seconds) above which a paused element is re-seeked (about one frame). */
const DRIFT_PAUSED_S = 0.02;
/** Minimum gap between two playing seeks of the same element (ms). Doubles up to the maximum. */
const SEEK_GAP_MS = 200;
const SEEK_GAP_MAX_MS = 2000;
/** Upper bound for a measured seek cost (ms), so one slow seek cannot run away. */
const SEEK_COST_MAX_MS = 1000;
/** Step interval for frozen, reversed and reverse-shuttled layers (ms, about 12 Hz). */
const STEP_MS = 83;
/** A frame whose step takes longer than this counts as dropped (ms). */
const DROP_STEP_MS = 20;
/** Window of the dropped-frame ratio (ms). */
const DROP_WINDOW_MS = 1000;
/** Tick interval of the audio-only loop while the window is hidden (ms). */
const HIDDEN_TICK_MS = 100;
/** Refresh interval of the audio gain curves (ms). */
const AUDIO_REFRESH_MS = 250;
/** Meter update interval (ms). */
const LEVELS_MS = 100;
/** Prefetch scan interval (ms). */
const PREFETCH_MS = 250;
/** How far ahead to prefetch (seconds). */
const PREFETCH_HORIZON_S = 2;
/** Span of the gain curve handed to the audio graph (seconds) and its sample step. */
const RAMP_SPAN_S = 0.5;
const RAMP_STEP_S = 0.02;
/** Debounce for redraws requested by seeks and load events (ms). */
const REDRAW_DEBOUNCE_MS = 30;

/** Quiet time before the unplayable-audio-track toast is shown, so one media gets one toast. */
const TRACK_TOAST_DEBOUNCE_MS = 150;
/** Smallest canvas side (pixels). */
const MIN_CANVAS_PX = 64;
/** Message of a codec the webview cannot decode (no video size after metadata). */
const CODEC_MESSAGE = 'This video codec cannot be decoded by WebView2 (HEVC?)';

const QUALITY_SCALE: Record<PreviewQuality, number> = { full: 1, half: 0.5, quarter: 0.25 };

const SILENT_LEVELS: { master: number; tracks: number[] } = { master: -120, tracks: [] };

/** One pooled video element. */
interface ElementEntry {
  mediaId: string;
  el: HTMLVideoElement;
  /** True once the playable URL has been assigned to `el`. */
  hasSrc: boolean;
  lastUsed: number;
  /** performance.now() of the last seek assigned to `el`. */
  lastSeekMs: number;
  /** performance.now() when the seek in flight was assigned, or null. */
  seekStartMs: number | null;
  /** Measured duration of the last completed seek (ms). Used as lead when re-seeking while playing. */
  seekCostMs: number;
  /** Current minimum gap between playing re-seeks (ms). */
  gapMs: number;
}

/** What one media id must show or sound at the current time. */
interface MediaNeed {
  /** Source time in ticks. */
  src: Tick;
  speed: number;
  reverse: boolean;
}

function messageOf(err: unknown): string {
  if (err instanceof Error && err.message) return err.message;
  if (typeof err === 'string' && err) return err;
  return 'unknown error';
}

function mediaErrorText(el: HTMLMediaElement): string {
  const e = el.error;
  if (!e) return 'playback error';
  return e.message ? e.message : `media error code ${e.code}`;
}

/** Clamps a rate magnitude to the accepted range. */
function clampMagnitude(m: number): number {
  if (m < MIN_RATE) return MIN_RATE;
  if (m > MAX_RATE) return MAX_RATE;
  return m;
}

/** Element playback rate. Non-positive or non-finite rates become 1. */
function clampRate(rate: number | undefined): number {
  if (rate === undefined || !Number.isFinite(rate) || rate <= 0) return 1;
  return clampMagnitude(rate);
}

/** Timeline rate with sign: negative rates reverse. Zero and non-finite rates become 1. */
function signedRate(rate: number | undefined): number {
  if (rate === undefined || !Number.isFinite(rate) || rate === 0) return 1;
  const mag = clampMagnitude(Math.abs(rate));
  return rate < 0 ? -mag : mag;
}

function clampTo(plan: RenderPlan, t: Tick): Tick {
  if (t < plan.start) return plan.start;
  if (t > plan.end) return plan.end;
  return t;
}

function cssColor(c: Rgba): string {
  return `rgba(${c.r}, ${c.g}, ${c.b}, ${(c.a / 255).toFixed(3)})`;
}

function notifyAll<T>(set: Set<(value: T) => void>, value: T): void {
  for (const cb of set) {
    try {
      cb(value);
    } catch {
      // A failing listener must not break the frame loop or other listeners.
    }
  }
}

/** Sets the preload hint only when it changes. */
function setPreload(entry: ElementEntry, value: 'metadata' | 'auto'): void {
  if (entry.el.preload !== value) entry.el.preload = value;
}

/** Detaches every handler of an image and releases its source. */
function disposeImage(img: HTMLImageElement): void {
  img.onload = null;
  img.onerror = null;
  img.src = '';
}

class Engine implements PreviewEngine {
  private readonly mediaUrl: (mediaId: string) => Promise<string>;
  private readonly clock = createClock();
  private readonly audio = new AudioGraph();
  private readonly sources: SourceFrames;

  private readonly elements = new Map<string, ElementEntry>();
  /** Resolved URL per media id, or null when resolution failed. */
  private readonly urls = new Map<string, Promise<string | null>>();
  private readonly images = new Map<string, HTMLImageElement>();
  /** Media that failed, with the reason. Cleared for a media id when its file changes. */
  private readonly failed = new Map<string, string>();
  /** Last segment start prefetched per media id. */
  private readonly prefetched = new Map<string, Tick>();
  /** Media ids the last prefetch scan wants loaded (preload 'auto'). */
  private prefetchIds = new Set<string>();
  /** Media ids needed by the current segment. Never evicted. */
  private keepIds = new Set<string>();
  /** Audio source keys (clip ids) that currently have a gain curve. */
  private activeKeys = new Set<string>();
  /** Audio source keys that have ever had a chain. Chains of removed clips are dropped. */
  private readonly chainKeys = new Set<string>();
  /** File fingerprint per media id as of the last setProject. */
  private readonly mediaKeys = new Map<string, string>();
  /** Transform overrides per clip id (painting only). */
  private readonly overrides = new Map<string, Transform>();
  /** Unplayable audio track indices per media id, collected until the debounced toast fires. */
  private readonly pendingTracks = new Map<string, Set<number>>();
  /** Media ids already warned about this session (one toast per media). */
  private readonly warnedMedia = new Set<string>();
  private trackToastTimer: ReturnType<typeof setTimeout> | null = null;

  private readonly timeCbs = new Set<(t: Tick) => void>();
  private readonly levelCbs = new Set<(l: { master: number; tracks: number[] }) => void>();
  private readonly errorCbs = new Set<(message: string) => void>();
  private readonly endedCbs = new Set<() => void>();
  private readonly droppedCbs = new Set<(ratio: number) => void>();

  private canvas: HTMLCanvasElement | null = null;
  private ctx2d: CanvasRenderingContext2D | null = null;
  private project: Project | null = null;
  private plan: RenderPlan | null = null;
  private quality: PreviewQuality = 'full';
  /** Signed timeline rate used while the clock runs. Negative rates play in reverse. */
  private rate = 1;
  private segStart: Tick | null = null;
  private lastAudioMs = 0;
  private lastLevelsMs = 0;
  private lastPrefetchMs = 0;
  private rafId: number | null = null;
  private redrawTimer: ReturnType<typeof setTimeout> | null = null;
  private hiddenTimer: ReturnType<typeof setInterval> | null = null;
  private reportedGeneric = false;
  private destroyed = false;
  /** Set when a source frame was not ready during the current paint. */
  private frameMissed = false;
  private dropWindowStart = 0;
  private dropFrames = 0;
  private dropCount = 0;

  constructor(opts: PreviewEngineOptions) {
    this.mediaUrl = opts.mediaUrl;
    this.sources = {
      video: (mediaId) => this.frameForPaint(mediaId),
      image: (mediaId) => this.imageForPaint(mediaId),
      failed: (mediaId) => this.failed.has(mediaId),
    };
    // The clock follows the audio device only while the context runs (see onAudioState).
    this.audio.ctx.onstatechange = () => this.onAudioState();
    document.addEventListener('visibilitychange', this.onVisibility);
  }

  attach(canvas: HTMLCanvasElement): void {
    if (this.destroyed) return;
    this.canvas = canvas;
    const ctx = canvas.getContext('2d');
    if (!ctx) {
      this.ctx2d = null;
      this.reportError(new Error('canvas 2D context is unavailable'));
      return;
    }
    this.ctx2d = ctx;
    this.applyCanvasSize();
    this.scheduleRedraw();
  }

  setProject(project: Project, plan: RenderPlan): void {
    if (this.destroyed) return;
    this.project = project;
    this.plan = plan;
    this.syncMediaKeys(project);
    this.dropStaleChains(plan);
    this.reportedGeneric = false;
    this.prefetched.clear();
    this.segStart = null;
    this.applyCanvasSize();
    if (!this.clock.isRunning()) {
      const now = this.clock.now();
      const clamped = clampTo(plan, now);
      if (clamped !== now) this.setClockPosition(clamped);
    }
    this.scheduleRedraw();
  }

  seek(t: Tick): void {
    if (this.destroyed) return;
    const plan = this.plan;
    if (!plan) return;
    const target = clampTo(plan, t);
    this.setClockPosition(target);
    this.prefetched.clear();
    this.segStart = null;
    this.emitTime(target);
    this.scheduleRedraw();
  }

  play(rate?: number): void {
    if (this.destroyed) return;
    const plan = this.plan;
    if (!plan) return;
    this.rate = signedRate(rate);
    const ctx = this.audio.ctx;
    void this.audio.resume().then(() => {
      if (!this.destroyed && ctx.state === 'running') this.clock.attachAudio(ctx);
    });
    if (this.clock.isRunning()) {
      this.clock.setRate(this.rate);
      this.segStart = null;
      this.resetDropWindow(performance.now());
      return;
    }
    let from = this.clock.now();
    if (this.rate < 0) {
      // Reverse: start just before the end, and do nothing when already at the start.
      if (from >= plan.end) from = plan.end - 1;
      if (from <= plan.start) return;
    } else if (from >= plan.end || from < plan.start) {
      from = plan.start;
    }
    this.clock.start(from, this.rate);
    this.segStart = null;
    this.lastAudioMs = 0;
    this.resetDropWindow(performance.now());
    this.updateHiddenTimer();
    if (this.rafId === null) this.rafId = requestAnimationFrame(this.loop);
  }

  pause(): void {
    if (this.destroyed) return;
    const wasRunning = this.clock.isRunning();
    const t = this.stopPlayback();
    if (wasRunning) {
      this.emitLevels(SILENT_LEVELS);
      this.emitTime(t);
    }
    this.scheduleRedraw();
  }

  isPlaying(): boolean {
    return this.clock.isRunning();
  }

  setMasterVolume(linear: number): void {
    if (this.destroyed) return;
    this.audio.setMaster(linear);
  }

  setQuality(q: PreviewQuality): void {
    if (this.destroyed) return;
    this.quality = q;
    this.applyCanvasSize();
    this.scheduleRedraw();
  }

  onTime(cb: (t: Tick) => void): () => void {
    this.timeCbs.add(cb);
    return () => {
      this.timeCbs.delete(cb);
    };
  }

  onLevels(cb: (l: { master: number; tracks: number[] }) => void): () => void {
    this.levelCbs.add(cb);
    return () => {
      this.levelCbs.delete(cb);
    };
  }

  onError(cb: (message: string) => void): () => void {
    this.errorCbs.add(cb);
    return () => {
      this.errorCbs.delete(cb);
    };
  }

  onEnded(cb: () => void): () => void {
    this.endedCbs.add(cb);
    return () => {
      this.endedCbs.delete(cb);
    };
  }

  onDroppedFrames(cb: (ratio: number) => void): () => void {
    this.droppedCbs.add(cb);
    return () => {
      this.droppedCbs.delete(cb);
    };
  }

  setTransformOverride(clipId: string, transform: Transform | null): void {
    if (this.destroyed) return;
    if (transform === null) this.overrides.delete(clipId);
    else this.overrides.set(clipId, transform);
    this.scheduleRedraw();
  }

  destroy(): void {
    if (this.destroyed) return;
    this.pause();
    this.destroyed = true;
    document.removeEventListener('visibilitychange', this.onVisibility);
    this.updateHiddenTimer();
    this.audio.ctx.onstatechange = null;
    if (this.redrawTimer !== null) {
      clearTimeout(this.redrawTimer);
      this.redrawTimer = null;
    }
    for (const entry of this.elements.values()) this.disposeEntry(entry);
    this.elements.clear();
    for (const img of this.images.values()) disposeImage(img);
    this.images.clear();
    this.urls.clear();
    this.failed.clear();
    this.prefetched.clear();
    this.prefetchIds = new Set<string>();
    this.overrides.clear();
    this.mediaKeys.clear();
    this.chainKeys.clear();
    if (this.trackToastTimer !== null) clearTimeout(this.trackToastTimer);
    this.trackToastTimer = null;
    this.pendingTracks.clear();
    this.warnedMedia.clear();
    this.audio.close();
    this.timeCbs.clear();
    this.levelCbs.clear();
    this.errorCbs.clear();
    this.endedCbs.clear();
    this.droppedCbs.clear();
    this.canvas = null;
    this.ctx2d = null;
    this.project = null;
    this.plan = null;
  }

  // ---- clock and playback state ------------------------------------------------------------

  /** Attaches the audio clock while the context runs, and falls back to the wall clock otherwise. */
  private onAudioState(): void {
    if (this.destroyed) return;
    this.clock.attachAudio(this.audio.ctx.state === 'running' ? this.audio.ctx : null);
  }

  /** Visibility change: the frame loop stops when hidden, so audio-only ticks take over. */
  private readonly onVisibility = (): void => {
    if (this.destroyed) return;
    this.updateHiddenTimer();
    if (!this.hidden() && this.clock.isRunning() && this.rafId === null) {
      this.rafId = requestAnimationFrame(this.loop);
    }
  };

  private hidden(): boolean {
    return typeof document !== 'undefined' && document.visibilityState === 'hidden';
  }

  /** Runs the audio-only tick while the window is hidden and the clock runs. */
  private updateHiddenTimer(): void {
    const want = !this.destroyed && this.hidden() && this.clock.isRunning();
    if (want && this.hiddenTimer === null) {
      this.hiddenTimer = setInterval(() => this.step(false), HIDDEN_TICK_MS);
    } else if (!want && this.hiddenTimer !== null) {
      clearInterval(this.hiddenTimer);
      this.hiddenTimer = null;
    }
  }

  /** Stops the clock, the frame loop, the elements and the audio. Returns the stop tick. */
  private stopPlayback(): Tick {
    const t = this.clock.stop();
    if (this.rafId !== null) {
      cancelAnimationFrame(this.rafId);
      this.rafId = null;
    }
    this.pauseElements();
    this.silenceKeys();
    this.updateHiddenTimer();
    return t;
  }

  /** Stops at `at`, optionally notifying the end listeners first. Emits the time once. */
  private stopAt(at: Tick, ended: boolean): void {
    this.stopPlayback();
    this.emitLevels(SILENT_LEVELS);
    if (ended) {
      for (const cb of this.endedCbs) {
        try {
          cb();
        } catch {
          // A failing listener must not break the stop.
        }
      }
    }
    this.setClockPosition(at);
    this.emitTime(at);
    this.scheduleRedraw();
  }

  // ---- frame loop -------------------------------------------------------------------------

  /** requestAnimationFrame callback. Never throws. */
  private readonly loop = (): void => {
    this.rafId = null;
    if (this.destroyed || !this.clock.isRunning()) return;
    this.step(true);
    if (!this.destroyed && this.clock.isRunning() && this.rafId === null) {
      this.rafId = requestAnimationFrame(this.loop);
    }
  };

  /**
   * One playing tick: sync media and audio, paint when `paint` is set, report time, meters and
   * prefetch. With `paint` false (hidden window) only the media, audio and reports run. Never throws.
   */
  private step(paint: boolean): void {
    const started = performance.now();
    try {
      const plan = this.plan;
      if (!plan) {
        this.pause();
        return;
      }
      const t = this.clock.now();
      if (this.rate < 0) {
        if (t <= plan.start) {
          this.stopAt(plan.start, false);
          return;
        }
      } else if (t >= plan.end) {
        this.stopAt(plan.end, true);
        return;
      }
      const now = performance.now();
      const seg = segmentAt(plan, t);
      if (seg) {
        this.syncMedia(seg, t, true, now);
        if (this.segStart !== seg.start || now - this.lastAudioMs >= AUDIO_REFRESH_MS) {
          this.syncAudio(seg, t);
          this.lastAudioMs = now;
          this.segStart = seg.start;
        }
      }
      if (paint) {
        this.frameMissed = false;
        if (seg) this.paintSegment(seg, t);
        else this.paintBlank();
        this.countFrame(this.frameMissed || performance.now() - started > DROP_STEP_MS, now);
      }
      this.emitTime(t);
      if (now - this.lastLevelsMs >= LEVELS_MS) {
        this.lastLevelsMs = now;
        this.emitLevels(this.audio.levels());
      }
      if (this.rate > 0 && now - this.lastPrefetchMs >= PREFETCH_MS) {
        this.lastPrefetchMs = now;
        this.prefetch(plan, t);
      }
    } catch (err) {
      this.reportError(err);
    }
  }

  /** Records one playing frame and emits the dropped-frame ratio once per window. */
  private countFrame(dropped: boolean, now: number): void {
    this.dropFrames++;
    if (dropped) this.dropCount++;
    if (now - this.dropWindowStart < DROP_WINDOW_MS) return;
    const ratio = this.dropCount / this.dropFrames;
    this.dropWindowStart = now;
    this.dropFrames = 0;
    this.dropCount = 0;
    notifyAll(this.droppedCbs, ratio);
  }

  private resetDropWindow(now: number): void {
    this.dropWindowStart = now;
    this.dropFrames = 0;
    this.dropCount = 0;
  }

  /**
   * Redraws the frame at `t` while paused, after a seek or a load event. Positions the needed
   * elements without playing them. Debounced through scheduleRedraw. At or past the end of a
   * non-empty plan, the last frame is shown.
   */
  private paintAt(t: Tick): void {
    try {
      const plan = this.plan;
      if (!plan || !this.ctx2d) return;
      const at = t >= plan.end && plan.end > plan.start ? plan.end - 1 : t;
      const seg = segmentAt(plan, at);
      if (seg) {
        this.syncMedia(seg, at, false, performance.now());
        this.paintSegment(seg, at);
      } else {
        this.paintBlank();
      }
    } catch (err) {
      this.reportError(err);
    }
  }

  private scheduleRedraw(): void {
    if (this.destroyed || this.redrawTimer !== null) return;
    this.redrawTimer = setTimeout(() => {
      this.redrawTimer = null;
      if (!this.destroyed && !this.clock.isRunning()) this.paintAt(this.clock.now());
    }, REDRAW_DEBOUNCE_MS);
  }

  /** Sets the clock position without changing whether it runs. */
  private setClockPosition(t: Tick): void {
    const running = this.clock.isRunning();
    this.clock.stop();
    this.clock.start(t, running ? this.rate : 1);
    if (!running) this.clock.stop();
  }

  // ---- canvas -----------------------------------------------------------------------------

  private applyCanvasSize(): void {
    const canvas = this.canvas;
    const project = this.project;
    if (!canvas || !project) return;
    const scale = QUALITY_SCALE[this.quality];
    const w = Math.max(MIN_CANVAS_PX, Math.round(project.canvas.width * scale));
    const h = Math.max(MIN_CANVAS_PX, Math.round(project.canvas.height * scale));
    if (canvas.width !== w) canvas.width = w;
    if (canvas.height !== h) canvas.height = h;
  }

  private paintSegment(seg: PlanSegment, t: Tick): void {
    const ctx = this.ctx2d;
    const project = this.project;
    const canvas = this.canvas;
    if (!ctx || !project || !canvas) return;
    composeFrame(
      ctx,
      canvas.width,
      canvas.height,
      project,
      this.overrides.size === 0 ? seg : this.withOverrides(seg),
      t,
      this.sources,
      QUALITY_SCALE[this.quality],
    );
  }

  /** Copy of the segment whose overridden clips carry the override transform. */
  private withOverrides(seg: PlanSegment): PlanSegment {
    return {
      ...seg,
      video: seg.video.map((layer) => {
        const transform = this.overrides.get(layer.clip_id);
        return transform ? { ...layer, transform } : layer;
      }),
    };
  }

  /** Fills the canvas with the project background (no segment covers the time). */
  private paintBlank(): void {
    const ctx = this.ctx2d;
    const project = this.project;
    const canvas = this.canvas;
    if (!ctx || !project || !canvas) return;
    ctx.fillStyle = cssColor(project.canvas.background);
    ctx.fillRect(0, 0, canvas.width, canvas.height);
  }

  private frameFor(mediaId: string): HTMLVideoElement | null {
    const entry = this.elements.get(mediaId);
    if (!entry || !entry.hasSrc) return null;
    return entry.el.readyState >= 2 ? entry.el : null;
  }

  /** Source lookup used while painting: records a miss for the dropped-frame count. */
  private frameForPaint(mediaId: string): HTMLVideoElement | null {
    const el = this.frameFor(mediaId);
    if (el === null) this.frameMissed = true;
    return el;
  }

  private imageFor(mediaId: string): HTMLImageElement | null {
    if (this.failed.has(mediaId) || this.destroyed) return null;
    let img = this.images.get(mediaId);
    if (!img) {
      const created = new Image();
      // Web Audio and canvas readback need CORS: the flag must be set before the source.
      created.crossOrigin = 'anonymous';
      created.onload = () => this.scheduleRedraw();
      created.onerror = () => this.failMedia(mediaId, 'image failed to load');
      this.images.set(mediaId, created);
      img = created;
      void this.urlFor(mediaId).then((url) => {
        if (url !== null && !this.destroyed && this.images.get(mediaId) === created) {
          created.src = url;
        }
      });
    }
    return img.complete && img.naturalWidth > 0 ? img : null;
  }

  private imageForPaint(mediaId: string): HTMLImageElement | null {
    const img = this.imageFor(mediaId);
    if (img === null) this.frameMissed = true;
    return img;
  }

  // ---- media pool -------------------------------------------------------------------------

  /** Resolves the URL of a media id once. The promise never rejects; failures mark the media. */
  private urlFor(mediaId: string): Promise<string | null> {
    let p = this.urls.get(mediaId);
    if (!p) {
      p = Promise.resolve()
        .then(() => this.mediaUrl(mediaId))
        .then(
          (url) => url,
          (err: unknown) => {
            this.failMedia(mediaId, err);
            return null;
          },
        );
      this.urls.set(mediaId, p);
    }
    return p;
  }

  /** Returns the pooled element for a media id, creating it (and its audio source) if needed. */
  private ensureElement(mediaId: string, now: number): ElementEntry | null {
    if (this.failed.has(mediaId) || this.destroyed) return null;
    const existing = this.elements.get(mediaId);
    if (existing) {
      existing.lastUsed = now;
      return existing;
    }
    this.evictIfFull();

    const el = document.createElement('video');
    el.preload = 'metadata';
    el.playsInline = true;
    el.muted = false;
    el.volume = 1;
    // Export resamples, so pitch follows speed in the preview too.
    el.preservesPitch = false;
    // Media comes from another origin. Web Audio needs CORS or the source is silent.
    el.crossOrigin = 'anonymous';
    const entry: ElementEntry = {
      mediaId,
      el,
      hasSrc: false,
      lastUsed: now,
      lastSeekMs: 0,
      seekStartMs: null,
      seekCostMs: 0,
      gapMs: SEEK_GAP_MS,
    };
    el.onseeked = () => {
      this.onSeeked(entry);
      this.scheduleRedraw();
    };
    el.onloadeddata = () => this.scheduleRedraw();
    el.onloadedmetadata = () => this.checkDecodable(mediaId, el);
    el.onerror = () => this.failMedia(mediaId, mediaErrorText(el));
    this.elements.set(mediaId, entry);

    try {
      this.audio.attach(mediaId, el);
    } catch (err) {
      this.failMedia(mediaId, err);
      return null;
    }

    void this.urlFor(mediaId).then((url) => {
      if (url === null || this.destroyed) return;
      if (this.elements.get(mediaId) !== entry) return;
      entry.hasSrc = true;
      el.src = url;
    });
    return entry;
  }

  /** Records the cost of a completed seek. */
  private onSeeked(entry: ElementEntry): void {
    if (entry.seekStartMs === null) return;
    const cost = performance.now() - entry.seekStartMs;
    entry.seekCostMs = Math.min(SEEK_COST_MAX_MS, Math.max(0, cost));
    entry.seekStartMs = null;
  }

  /** Assigns a seek and records when it started, so its cost can be measured. */
  private seekTo(entry: ElementEntry, sec: number, now: number): void {
    entry.el.currentTime = sec;
    entry.lastSeekMs = now;
    entry.seekStartMs = performance.now();
  }

  /** Metadata without a video size means the codec is not decodable (WebView2 without HEVC). */
  private checkDecodable(mediaId: string, el: HTMLVideoElement): void {
    if (this.destroyed || this.elements.get(mediaId)?.el !== el) return;
    const kind = this.project?.media.find((m) => m.id === mediaId)?.kind;
    if (kind === 'video' && el.videoWidth === 0) {
      this.failMedia(mediaId, CODEC_MESSAGE);
      return;
    }
    this.scheduleRedraw();
  }

  /** True when one more element fits, or an unneeded element can be evicted to make room. */
  private hasRoom(): boolean {
    if (this.elements.size < MAX_ELEMENTS) return true;
    for (const entry of this.elements.values()) {
      if (!this.keepIds.has(entry.mediaId)) return true;
    }
    return false;
  }

  /** Makes room for one more element by dropping the least recently used unneeded one. */
  private evictIfFull(): void {
    if (this.elements.size < MAX_ELEMENTS) return;
    let victim: ElementEntry | null = null;
    for (const entry of this.elements.values()) {
      if (this.keepIds.has(entry.mediaId)) continue;
      if (victim === null || entry.lastUsed < victim.lastUsed) victim = entry;
    }
    if (victim === null) return;
    this.disposeEntry(victim);
    this.audio.detach(victim.mediaId);
    this.elements.delete(victim.mediaId);
  }

  private disposeEntry(entry: ElementEntry): void {
    const el = entry.el;
    el.onseeked = null;
    el.onloadeddata = null;
    el.onloadedmetadata = null;
    el.onerror = null;
    try {
      el.pause();
      if (entry.hasSrc) {
        el.removeAttribute('src');
        el.load();
      }
    } catch {
      // The element may already be detached from its media: nothing to undo.
    }
    entry.hasSrc = false;
  }

  /** Drops the element, its audio source and its image of a media id. Keeps failure state. */
  private releaseMedia(mediaId: string): void {
    const entry = this.elements.get(mediaId);
    if (entry) {
      this.disposeEntry(entry);
      this.elements.delete(mediaId);
    }
    this.audio.detach(mediaId);
    const img = this.images.get(mediaId);
    if (img) {
      disposeImage(img);
      this.images.delete(mediaId);
    }
  }

  /** Forgets everything known about a media id, so it is resolved and decoded again. */
  private dropMedia(mediaId: string): void {
    this.failed.delete(mediaId);
    this.urls.delete(mediaId);
    this.prefetched.delete(mediaId);
    this.releaseMedia(mediaId);
  }

  /**
   * Compares the file fingerprint of every media with the last setProject. Changed, new or removed
   * media are dropped so that they are resolved again. Unchanged media keep their failure state.
   */
  private syncMediaKeys(project: Project): void {
    const next = new Map<string, string>();
    for (const m of project.media) {
      next.set(m.id, `${m.path}|${m.fingerprint.size}|${m.fingerprint.mtime_ms}|${m.missing}`);
    }
    const stale = new Set<string>();
    for (const [id, key] of this.mediaKeys) {
      if (next.get(id) !== key) stale.add(id);
    }
    for (const [id, key] of next) {
      if (this.mediaKeys.get(id) !== key) stale.add(id);
    }
    for (const id of stale) this.dropMedia(id);
    this.mediaKeys.clear();
    for (const [id, key] of next) this.mediaKeys.set(id, key);
  }

  /** Removes the audio chains of clips that the new plan no longer has. */
  private dropStaleChains(plan: RenderPlan): void {
    const live = new Set<string>();
    for (const seg of plan.segments) {
      for (const src of seg.audio) live.add(src.clip_id);
    }
    for (const key of this.chainKeys) {
      if (live.has(key)) continue;
      this.audio.removeSource(key);
      this.chainKeys.delete(key);
      this.activeKeys.delete(key);
    }
  }

  /** Marks a media id unavailable, drops its element and reports the failure once. */
  private failMedia(mediaId: string, err: unknown): void {
    if (this.destroyed || this.failed.has(mediaId)) return;
    const reason = typeof err === 'string' ? err : messageOf(err);
    this.failed.set(mediaId, reason);
    this.releaseMedia(mediaId);
    const name = this.project?.media.find((m) => m.id === mediaId)?.name ?? mediaId;
    this.emitError(`Media "${name}" is unavailable: ${reason}`);
  }

  private pauseElements(): void {
    for (const entry of this.elements.values()) {
      if (!entry.el.paused) entry.el.pause();
    }
  }

  // ---- positioning ------------------------------------------------------------------------

  /**
   * The media each source needs at `t`. A video layer wins over an audio source of the same media.
   * Audio sources on tracks other than the default stream are skipped (reported once per pair).
   */
  private collectNeeds(seg: PlanSegment, t: Tick): Map<string, MediaNeed> {
    const needs = new Map<string, MediaNeed>();
    for (const layer of seg.video) {
      if (layer.kind !== 'video' || layer.media_id === null) continue;
      if (needs.has(layer.media_id)) continue;
      needs.set(layer.media_id, {
        src: sourceTimeFor(layer, seg.start, t),
        speed: layer.speed,
        reverse: layer.reverse,
      });
    }
    for (const src of seg.audio) {
      if (src.source_track !== 0) {
        this.reportUnplayableTrack(src);
        continue;
      }
      if (needs.has(src.media_id)) continue;
      needs.set(src.media_id, {
        src: sourceTimeFor(src, seg.start, t),
        speed: src.speed,
        reverse: src.reverse,
      });
    }
    return needs;
  }

  /**
   * Collects an audio track that cannot be previewed. Tracks of one media are gathered and reported
   * in a single debounced toast, and each media is warned about at most once per session.
   */
  private reportUnplayableTrack(src: AudioSource): void {
    if (this.destroyed || this.warnedMedia.has(src.media_id)) return;
    let tracks = this.pendingTracks.get(src.media_id);
    if (!tracks) {
      tracks = new Set<number>();
      this.pendingTracks.set(src.media_id, tracks);
    }
    if (tracks.has(src.source_track)) return;
    tracks.add(src.source_track);
    if (this.trackToastTimer !== null) clearTimeout(this.trackToastTimer);
    this.trackToastTimer = setTimeout(() => this.flushTrackToasts(), TRACK_TOAST_DEBOUNCE_MS);
  }

  /** Emits one toast per media with the collected unplayable audio tracks. */
  private flushTrackToasts(): void {
    this.trackToastTimer = null;
    if (this.destroyed) return;
    for (const [mediaId, tracks] of this.pendingTracks) {
      this.warnedMedia.add(mediaId);
      const name = this.project?.media.find((m) => m.id === mediaId)?.name ?? mediaId;
      const list = [...tracks].sort((a, b) => a - b).join(', ');
      const noun = tracks.size === 1 ? 'audio track' : 'audio tracks';
      this.emitError(
        `${name}: ${noun} ${list} cannot be previewed yet (only the default stream plays in preview; export includes every track)`,
      );
    }
    this.pendingTracks.clear();
  }

  /**
   * Positions the elements that the segment needs and pauses the others. While playing, elements
   * run at their layer speed times the timeline rate and are nudged or re-seeked when they drift.
   * While paused they are seeked to the exact frame and never play.
   */
  private syncMedia(seg: PlanSegment, t: Tick, playing: boolean, now: number): void {
    const needs = this.collectNeeds(seg, t);
    this.keepIds = new Set(needs.keys());
    for (const entry of this.elements.values()) {
      if (needs.has(entry.mediaId)) continue;
      if (!entry.el.paused) entry.el.pause();
      setPreload(entry, this.prefetchIds.has(entry.mediaId) ? 'auto' : 'metadata');
    }
    for (const [mediaId, need] of needs) {
      const entry = this.ensureElement(mediaId, now);
      if (!entry) continue;
      setPreload(entry, 'auto');
      this.positionElement(entry, need, playing, now);
    }
  }

  private positionElement(entry: ElementEntry, need: MediaNeed, playing: boolean, now: number): void {
    if (!entry.hasSrc) return;
    const el = entry.el;
    const target = tickToSec(need.src);

    if (need.speed <= 0 || need.reverse || (playing && this.rate < 0)) {
      // Frozen, reversed or reverse-shuttled: cannot play backwards, so step the frame.
      if (!el.paused) el.pause();
      const stepOk = !playing || now - entry.lastSeekMs >= STEP_MS;
      if (stepOk && !el.seeking && Math.abs(el.currentTime - target) > DRIFT_PAUSED_S) {
        this.seekTo(entry, target, now);
      }
      return;
    }

    const mag = playing ? Math.abs(this.rate) : 1;
    const nominal = clampRate(need.speed * mag);
    if (!playing) {
      if (el.playbackRate !== nominal) el.playbackRate = nominal;
      if (!el.paused) el.pause();
      if (!el.seeking && Math.abs(el.currentTime - target) > DRIFT_PAUSED_S) {
        this.seekTo(entry, target, now);
      }
      return;
    }

    // Playing: positive drift means the element is ahead of its source time.
    const drift = el.currentTime - target;
    const abs = Math.abs(drift);
    if (abs > DRIFT_SEEK_S && !el.seeking && now - entry.lastSeekMs >= entry.gapMs) {
      // Lead the target by the cost of the seek, since the element keeps playing while it loads.
      this.seekTo(entry, target + (entry.seekCostMs / 1000) * nominal, now);
      entry.gapMs = Math.min(SEEK_GAP_MAX_MS, entry.gapMs * 2);
      if (el.playbackRate !== nominal) el.playbackRate = nominal;
    } else if (abs > DRIFT_NUDGE_S) {
      const factor = drift > 0 ? 1 - NUDGE_FACTOR : 1 + NUDGE_FACTOR;
      const nudged = clampRate(nominal * factor);
      if (el.playbackRate !== nudged) el.playbackRate = nudged;
    } else {
      if (el.playbackRate !== nominal) el.playbackRate = nominal;
      entry.gapMs = SEEK_GAP_MS;
    }
    if (el.paused) this.playElement(el);
  }

  private playElement(el: HTMLVideoElement): void {
    void el.play().catch((err: unknown) => {
      if (err instanceof DOMException && err.name === 'AbortError') return;
      this.reportError(err);
    });
  }

  // ---- audio ------------------------------------------------------------------------------

  /**
   * Refreshes the gain curve of every default-stream audio source of the segment and silences the
   * others. Reverse playback is silent. Gain offsets are scaled to wall time by the timeline rate.
   */
  private syncAudio(seg: PlanSegment, t: Tick): void {
    if (this.rate < 0) {
      this.silenceKeys();
      return;
    }
    const at = this.audio.ctx.currentTime;
    const mag = Math.abs(this.rate);
    const seen = new Set<string>();
    for (const src of seg.audio) {
      if (src.source_track !== 0 || seen.has(src.clip_id)) continue;
      seen.add(src.clip_id);
      const ramp: GainPoint[] = gainRamp(src, t, t + secToTick(RAMP_SPAN_S), RAMP_STEP_S).map((p) => ({
        offsetSec: p.offsetSec / mag,
        gain: p.gain,
      }));
      this.chainKeys.add(src.clip_id);
      this.audio.setSourceParams(
        src.clip_id,
        src.media_id,
        src.track_index,
        ramp,
        src.audio.pan,
        src.audio.mono,
        at,
      );
    }
    for (const key of this.activeKeys) {
      if (!seen.has(key)) this.audio.silenceSource(key);
    }
    this.activeKeys = seen;
  }

  private silenceKeys(): void {
    for (const key of this.activeKeys) this.audio.silenceSource(key);
    this.activeKeys = new Set<string>();
  }

  // ---- prefetch ---------------------------------------------------------------------------

  /**
   * Prepares the elements of the segments that start within the prefetch horizon: creates them
   * (which starts URL resolution and buffering) and parks idle elements at their first source time.
   * Elements that are not needed keep preload 'metadata' unless a prefetch wants them.
   */
  private prefetch(plan: RenderPlan, t: Tick): void {
    const now = performance.now();
    const items = upcoming(plan, t, secToTick(PREFETCH_HORIZON_S));
    this.prefetchIds = new Set(items.map((item) => item.mediaId));
    for (const item of items) {
      if (this.keepIds.has(item.mediaId)) continue;
      if (this.prefetched.get(item.mediaId) === item.startsAt) continue;
      if (!this.elements.has(item.mediaId) && !this.hasRoom()) continue;
      const entry = this.ensureElement(item.mediaId, now);
      if (!entry) continue;
      setPreload(entry, 'auto');
      // The URL resolves asynchronously: retry on a later scan until the source is set.
      if (!entry.hasSrc) continue;
      this.prefetched.set(item.mediaId, item.startsAt);
      const target = tickToSec(item.srcTime);
      if (entry.el.paused && !entry.el.seeking && Math.abs(entry.el.currentTime - target) > DRIFT_PAUSED_S) {
        this.seekTo(entry, target, now);
      }
    }
  }

  // ---- reporting --------------------------------------------------------------------------

  private emitTime(t: Tick): void {
    notifyAll(this.timeCbs, t);
  }

  private emitLevels(l: { master: number; tracks: number[] }): void {
    notifyAll(this.levelCbs, l);
  }

  private emitError(message: string): void {
    notifyAll(this.errorCbs, message);
  }

  /** Reports an unexpected error once until the next setProject. */
  private reportError(err: unknown): void {
    if (this.reportedGeneric || this.destroyed) return;
    this.reportedGeneric = true;
    this.emitError(messageOf(err));
  }
}

/**
 * Creates a program monitor engine. Creates the AudioContext (suspended until `play`) but no
 * canvas or media elements; those are created on attach and on first use.
 */
export function createPreviewEngine(opts: PreviewEngineOptions): PreviewEngine {
  return new Engine(opts);
}
