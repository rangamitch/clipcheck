// Web Audio mixer for multi-track recordings. Contract: docs/impl/player-mvp.md.
// Pure gain math lives in ./model; this file only wires DOM media elements to the audio graph.

import { needsResync, trackGain, type TrackState } from './model';
import { applyOutput, DEFAULT_OUTPUT_ID, getSavedOutput, type SinkTarget } from './output';

export interface MixerDeps {
  /** Creates the AudioContext. Defaults to `new AudioContext()`; tests inject a fake. */
  createContext?: () => AudioContext;
}

/**
 * Routes each hidden `<audio>` element through its own GainNode into one destination.
 *
 * `createMediaElementSource` can be called only once per element, so an element can be
 * attached to a context only once. Player.svelte must create fresh audio elements for each
 * opened recording and never re-attach an element that was used before.
 *
 * The AudioContext is created lazily in `attach`, so constructing a mixer has no side effects.
 *
 * Output device: `attach` routes the context to the saved output (see ./output). The choice made
 * with `setOutputDevice` is kept, so a context created later uses it too. A bound video element
 * (`bindVideo`) is routed to the same device.
 */
export class TrackMixer {
  private readonly createContext: () => AudioContext;
  private ctx: AudioContext | null = null;
  private sources: MediaElementAudioSourceNode[] = [];
  private gains: GainNode[] = [];
  /** Output id chosen this session; null means use the saved output. */
  private sinkId: string | null = null;
  private video: HTMLMediaElement | null = null;

  constructor(deps: MixerDeps = {}) {
    this.createContext = deps.createContext ?? (() => new AudioContext());
  }

  /** True while a context and its per-track nodes exist. */
  get attached(): boolean {
    return this.ctx !== null;
  }

  /**
   * Replaces the current track set. Any previous set is detached first. An empty array leaves
   * the mixer detached and creates no context.
   */
  attach(elements: HTMLMediaElement[]): void {
    this.detach();
    if (elements.length === 0) return;

    const ctx = this.createContext();
    const sources: MediaElementAudioSourceNode[] = [];
    const gains: GainNode[] = [];
    try {
      for (const el of elements) {
        enableCorsForWebAudio(el);
        const source = ctx.createMediaElementSource(el);
        const gain = ctx.createGain();
        source.connect(gain);
        gain.connect(ctx.destination);
        sources.push(source);
        gains.push(gain);
      }
    } catch (err) {
      // Partial graph: tear down what was built, then surface the original error.
      for (const s of sources) safeDisconnect(s);
      for (const g of gains) safeDisconnect(g);
      closeQuietly(ctx);
      throw err;
    }
    this.ctx = ctx;
    this.sources = sources;
    this.gains = gains;
    // The context is created outside a user gesture; the output routing is independent of that.
    void applyOutput(ctx as unknown as SinkTarget, this.currentSinkId());
  }

  /**
   * Routes the mixer context, and the bound video element if any, to the output `id`
   * ('default' = system default device). The choice is kept for contexts created later.
   * Resolves true when every live target accepted it (true when none is live yet), false when
   * one failed or the engine has no setSinkId. Never rejects.
   */
  async setOutputDevice(id: string): Promise<boolean> {
    const next = id === '' ? DEFAULT_OUTPUT_ID : id;
    this.sinkId = next;
    const ctx = this.ctx;
    const video = this.video;
    const ctxOk = ctx ? await applyOutput(ctx as unknown as SinkTarget, next) : true;
    const videoOk = video ? await applyOutput(video as unknown as SinkTarget, next) : true;
    return ctxOk && videoOk;
  }

  /**
   * Binds the video element whose own audio path should follow the output device, or unbinds
   * it with null. Applies the current output at once.
   */
  bindVideo(el: HTMLMediaElement | null): void {
    this.video = el;
    if (el) void applyOutput(el as unknown as SinkTarget, this.currentSinkId());
  }

  private currentSinkId(): string {
    return this.sinkId ?? getSavedOutput();
  }

  /**
   * Sets each track's gain for the current states, volume and mute flag. Smoothed with a
   * 10 ms time constant to avoid clicks. A track with no matching state is silenced.
   * No-op when not attached.
   */
  apply(states: TrackState[], volume: number, muted: boolean): void {
    const ctx = this.ctx;
    if (!ctx) return;
    const now = ctx.currentTime;
    for (let i = 0; i < this.gains.length; i++) {
      const value = states[i] === undefined ? 0 : trackGain(states, i, volume, muted);
      this.gains[i].gain.setTargetAtTime(value, now, 0.01);
    }
  }

  /**
   * Resumes a suspended context. Browsers require this after a user gesture. Resolves at once
   * when not attached or already running.
   */
  async resume(): Promise<void> {
    const ctx = this.ctx;
    if (!ctx || ctx.state !== 'suspended') return;
    await ctx.resume();
  }

  /**
   * Disconnects every node and closes the context. Errors from individual nodes and from
   * `close` are swallowed. Safe to call more than once.
   */
  detach(): void {
    for (const s of this.sources) safeDisconnect(s);
    for (const g of this.gains) safeDisconnect(g);
    const ctx = this.ctx;
    this.ctx = null;
    this.sources = [];
    this.gains = [];
    if (ctx) closeQuietly(ctx);
  }
}

/**
 * Media from another origin (the clip:// protocol is http://clip.localhost while the app is served
 * from http://tauri.localhost) feeds a MediaElementAudioSourceNode silence unless the element
 * requests CORS. This sets crossOrigin before the load starts and restarts the load of the
 * current source so the setting applies. The media server must send Access-Control-Allow-Origin
 * for the load to succeed.
 */
function enableCorsForWebAudio(el: HTMLMediaElement): void {
  if (el.crossOrigin === 'anonymous') return;
  el.crossOrigin = 'anonymous';
  const src = el.currentSrc || el.src;
  if (!src) return;
  el.src = src;
  if (typeof el.load === 'function') el.load();
}

function safeDisconnect(node: AudioNode): void {
  try {
    node.disconnect();
  } catch {
    // Already disconnected or context closed: nothing to undo.
  }
}

function closeQuietly(ctx: AudioContext): void {
  try {
    ctx.close().catch(() => {});
  } catch {
    // close() threw synchronously (fake or broken context): ignore.
  }
}

/**
 * Keeps hidden audio elements in step with the video. Call from a timeupdate or
 * requestAnimationFrame loop. It does no per-call allocation beyond the loop itself.
 *
 * Mirrors `playbackRate`. Pauses the audio while the video is paused or ended. Plays the audio
 * while the video plays. Seeks any audio that drifts more than `thresholdS` from the video.
 */
/** Minimum gap between two hard seeks of the same audio element (ms). */
const RESYNC_MIN_GAP_MS = 1000;
const lastResync = new WeakMap<object, number>();

/**
 * Hard-seeks an audio element to the video time, but never one that is still seeking or was just
 * seeked: re-seeking a sidecar that has not caught up yet restarts its load and replays the same
 * fraction of a second over and over.
 */
function resyncAudio(a: HTMLMediaElement, videoT: number, thresholdS: number): void {
  if (!needsResync(videoT, a.currentTime, thresholdS)) return;
  if (a.seeking) return;
  const nowMs = Date.now();
  const last = lastResync.get(a);
  if (last !== undefined && nowMs - last < RESYNC_MIN_GAP_MS) return;
  lastResync.set(a, nowMs);
  a.currentTime = videoT;
}

export function syncAudioToVideo(
  video: HTMLMediaElement,
  audios: HTMLMediaElement[],
  thresholdS = 0.15,
): void {
  const rate = video.playbackRate;
  const stopped = video.paused || video.ended;
  const videoT = video.currentTime;
  for (let i = 0; i < audios.length; i++) {
    const a = audios[i];
    if (a.playbackRate !== rate) a.playbackRate = rate;
    if (stopped) {
      a.pause();
      resyncAudio(a, videoT, thresholdS);
    } else {
      resyncAudio(a, videoT, thresholdS);
      if (a.paused) void a.play().catch(() => {});
    }
  }
}
