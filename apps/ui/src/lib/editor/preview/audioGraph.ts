import type { GainPoint } from './scheduler';
import { panGains } from './scheduler';

// Web Audio graph for the program monitor. One AudioContext, one MediaElementAudioSourceNode per
// hidden audio element, and one chain per timeline audio source:
//
//   audio element source -> clip gain (2 ch) -> [mono stage] -> splitter -> L/R gains -> merger
//     -> track gain -> track meter -> master gain -> master meter -> destination
//
// The mono stage (used when ClipAudio.mono is set) averages L and R and writes the same value to
// both channels, as the export does. The pan stage is the constant-power law of `panGains` (see
// crates/timeline/src/audiomath.rs `pan_gains`) with no cross-feed between the channels.
//
// Gain curves come from the render plan through `gainRamp` (scheduler.ts). Gain smoothing follows
// the mixer conventions in ../../player/mixer.ts: a 10 ms time constant for static changes, and
// disconnect helpers that swallow errors from nodes that are already gone.
//
// Streams: only hidden audio elements are attached here, never the picture <video> elements (routing a
// video element through Web Audio stalled its frame delivery). Every stream, the default one (0, the
// original file) included, is played by its own audio element, attached under the key
// `<mediaId>#<stream>` (the `mediaId` argument of `attach` and `setSourceParams` is just a source
// key). A media element source that no chain uses is connected to nothing and is therefore silent.
//
// CORS: media is served from another origin (clip protocol). The element must request CORS
// (crossOrigin 'anonymous') before its load starts, or the source node outputs silence. The
// engine sets this when it creates an element. `attach` does not change it.

/** Level reported for silence, in dBFS. */
const SILENT_DB = -120;

/** Linear gain ceiling for the master fader (about +12 dB). */
const MAX_LINEAR = 4;

/** Time constant for static gain changes (seconds). Same value as the player mixer. */
const SMOOTH_S = 0.01;

/** Length of the click guard ramp when a gain curve starts or a source is silenced (seconds). */
const CLICK_RAMP_S = 0.005;

/** Length of the fade to silence in `silenceSource` (seconds). */
const SILENCE_S = 0.01;

/** Analyser window size (samples). */
const METER_FFT = 1024;

interface MediaEntry {
  el: HTMLMediaElement;
  node: MediaElementAudioSourceNode;
}

interface SourceChain {
  mediaId: string;
  /** Input of the chain. Stereo, explicit channel count. */
  clipGain: GainNode;
  /** Downmix to one channel, used only while `mono` is set. */
  monoDown: GainNode;
  /** Upmix back to two channels (L = R = mono). Fed only by `monoDown`. */
  monoUp: GainNode;
  /** Splits the stereo signal into the two pan channels. */
  splitter: ChannelSplitterNode;
  /** Left and right pan gains, one channel each. */
  panL: GainNode;
  panR: GainNode;
  /** Chain output, stereo. Connected to the track gain of `trackIndex`. */
  merger: ChannelMergerNode;
  trackIndex: number;
  mono: boolean;
}

interface TrackBus {
  gain: GainNode;
  meter: AnalyserNode;
  buffer: Float32Array<ArrayBuffer>;
}

/** Structural view of AudioParam.cancelAndHoldAtTime, which not every WebView2 build exposes. */
interface HoldingParam {
  cancelAndHoldAtTime?: (when: number) => unknown;
}

/** Converts a linear amplitude to dBFS, or the silence level for zero. */
function toDb(linear: number): number {
  if (!(linear > 0)) return SILENT_DB;
  const db = 20 * Math.log10(linear);
  return db < SILENT_DB ? SILENT_DB : db;
}

/** Peak absolute sample of a meter buffer, as linear amplitude. */
function peakOf(meter: AnalyserNode, buffer: Float32Array<ArrayBuffer>): number {
  meter.getFloatTimeDomainData(buffer);
  let peak = 0;
  for (let i = 0; i < buffer.length; i++) {
    const v = Math.abs(buffer[i]);
    if (v > peak) peak = v;
  }
  return peak;
}

function clampLinear(v: number): number {
  if (!Number.isFinite(v) || v < 0) return 0;
  return v > MAX_LINEAR ? MAX_LINEAR : v;
}

function safeDisconnect(node: AudioNode): void {
  try {
    node.disconnect();
  } catch {
    // Already disconnected or the context is closed: nothing to undo.
  }
}

/**
 * Holds the current value of a gain parameter at `at`, so a following ramp starts from the value
 * that is actually heard. Uses cancelAndHoldAtTime where available and otherwise falls back to
 * cancelling and pinning the value that the parameter reports now.
 */
function holdAt(param: AudioParam, at: number): void {
  const holder = param as unknown as HoldingParam;
  if (typeof holder.cancelAndHoldAtTime === 'function') {
    try {
      holder.cancelAndHoldAtTime(at);
      return;
    } catch {
      // Fall through to the generic path.
    }
  }
  param.cancelScheduledValues(at);
  param.setValueAtTime(param.value, at);
}

/** Every node of a chain, in graph order. */
function chainNodes(chain: SourceChain): AudioNode[] {
  return [chain.clipGain, chain.monoDown, chain.monoUp, chain.splitter, chain.panL, chain.panR, chain.merger];
}

/**
 * Wraps one AudioContext and the per-media and per-clip nodes of the program monitor.
 *
 * `attach` is idempotent for the same element. A different element for a media id replaces the
 * source node and reconnects every chain that uses that media id. `setSourceParams` creates a
 * chain on first use and reschedules its gain curve on every call. Chains are kept after
 * `silenceSource`, so a clip that returns reuses its nodes. `removeSource` drops a chain for good.
 *
 * All scheduling uses the context clock. Callers pass `atContextTime` (usually `ctx.currentTime`).
 */
export class AudioGraph {
  /** The underlying context. Created suspended until `resume` runs inside a user gesture. */
  readonly ctx: AudioContext;

  private readonly master: GainNode;
  private readonly masterMeter: AnalyserNode;
  private readonly masterBuffer: Float32Array<ArrayBuffer>;
  private readonly media = new Map<string, MediaEntry>();
  private readonly sourceByElement = new WeakMap<HTMLMediaElement, MediaElementAudioSourceNode>();
  private readonly chains = new Map<string, SourceChain>();
  private readonly tracks = new Map<number, TrackBus>();
  private closed = false;

  constructor() {
    // Default latency hint: 'playback' buffers more audio, which delays the media clock of every
    // routed element against the playhead clock (and made the drift correction fight the video).
    this.ctx = new AudioContext();
    this.master = this.ctx.createGain();
    this.masterMeter = this.ctx.createAnalyser();
    this.masterMeter.fftSize = METER_FFT;
    this.masterBuffer = new Float32Array(METER_FFT);
    this.master.connect(this.masterMeter);
    this.masterMeter.connect(this.ctx.destination);
  }

  /**
   * Creates (or reuses) the source node for the audio element `video` and makes it the source for `mediaId` (a source key).
   * Each element gets exactly one source node, since createMediaElementSource may run once per
   * element. Chains that already use `mediaId` are reconnected to the new node.
   */
  attach(mediaId: string, video: HTMLMediaElement): void {
    if (this.closed) return;
    const current = this.media.get(mediaId);
    if (current && current.el === video) return;

    let node = this.sourceByElement.get(video);
    if (!node) {
      node = this.ctx.createMediaElementSource(video);
      this.sourceByElement.set(video, node);
    }
    if (current) safeDisconnect(current.node);
    this.media.set(mediaId, { el: video, node });

    for (const chain of this.chains.values()) {
      if (chain.mediaId === mediaId) node.connect(chain.clipGain);
    }
  }

  /**
   * Disconnects the media element source of `mediaId` and forgets it. Chains that used the id keep
   * their nodes and stay silent until `attach` runs again for that id. Unknown ids are ignored.
   */
  detach(mediaId: string): void {
    if (this.closed) return;
    const entry = this.media.get(mediaId);
    if (!entry) return;
    safeDisconnect(entry.node);
    this.media.delete(mediaId);
  }

  /**
   * Schedules the gain curve, pan and mono flag of one timeline audio source. `key` is the clip id.
   * `ramp` points are relative to `atContextTime`; points in the past are clamped to now.
   * The curve is replaced from `atContextTime` on, so calling this every 250 ms is safe.
   * A source whose media is not attached yet is stored and connected when `attach` runs.
   */
  setSourceParams(
    key: string,
    mediaId: string,
    trackIndex: number,
    ramp: GainPoint[],
    pan: number,
    mono: boolean,
    atContextTime: number,
  ): void {
    if (this.closed) return;
    const chain = this.chainFor(key, mediaId);
    const at = Math.max(atContextTime, this.ctx.currentTime);

    const gain = chain.clipGain.gain;
    holdAt(gain, at);
    const first = ramp[0];
    if (first === undefined) {
      gain.linearRampToValueAtTime(0, at + CLICK_RAMP_S);
    } else {
      gain.linearRampToValueAtTime(clampLinear(first.gain), at + CLICK_RAMP_S);
      for (let i = 1; i < ramp.length; i++) {
        const p = ramp[i];
        const when = at + Math.max(0, p.offsetSec);
        gain.linearRampToValueAtTime(clampLinear(p.gain), when);
      }
    }

    const { l, r } = panGains(pan);
    chain.panL.gain.cancelScheduledValues(at);
    chain.panL.gain.setValueAtTime(l, at);
    chain.panR.gain.cancelScheduledValues(at);
    chain.panR.gain.setValueAtTime(r, at);

    if (chain.mono !== mono) {
      this.routeMono(chain, mono);
    }

    if (chain.trackIndex !== trackIndex) {
      // Only a chain already routed to a bus has anything to disconnect (-1 means never routed).
      if (chain.trackIndex !== -1) safeDisconnect(chain.merger);
      chain.merger.connect(this.trackBus(trackIndex).gain);
      chain.trackIndex = trackIndex;
    }
  }

  /** Fades the source of `key` to silence over about 10 ms. Unknown keys are ignored. */
  silenceSource(key: string): void {
    if (this.closed) return;
    const chain = this.chains.get(key);
    if (!chain) return;
    const now = this.ctx.currentTime;
    const gain = chain.clipGain.gain;
    holdAt(gain, now);
    gain.linearRampToValueAtTime(0, now + SILENCE_S);
  }

  /**
   * Disconnects and forgets the chain of `key`, including its scheduled gain. Unknown keys are
   * ignored. A later `setSourceParams` with the same key builds a new chain.
   */
  removeSource(key: string): void {
    if (this.closed) return;
    const chain = this.chains.get(key);
    if (!chain) return;
    const source = this.media.get(chain.mediaId);
    if (source) {
      try {
        source.node.disconnect(chain.clipGain);
      } catch {
        // Not connected: nothing to undo.
      }
    }
    chain.clipGain.gain.cancelScheduledValues(this.ctx.currentTime);
    for (const node of chainNodes(chain)) safeDisconnect(node);
    this.chains.delete(key);
  }

  /** Sets the master fader to a linear gain (0 = silence, 1 = unity, capped at about +12 dB). */
  setMaster(linear: number): void {
    if (this.closed) return;
    this.master.gain.setTargetAtTime(clampLinear(linear), this.ctx.currentTime, SMOOTH_S);
  }

  /**
   * Current meter readings. `master` is the peak of the master bus in dBFS. `tracks[i]` is the
   * peak of track index `i` (-120 for tracks with no bus yet). Silence reads -120.
   */
  levels(): { master: number; tracks: number[] } {
    if (this.closed) return { master: SILENT_DB, tracks: [] };
    const master = toDb(peakOf(this.masterMeter, this.masterBuffer));
    let maxIndex = -1;
    for (const index of this.tracks.keys()) if (index > maxIndex) maxIndex = index;
    const tracks: number[] = [];
    for (let i = 0; i <= maxIndex; i++) {
      const bus = this.tracks.get(i);
      tracks.push(bus ? toDb(peakOf(bus.meter, bus.buffer)) : SILENT_DB);
    }
    return { master, tracks };
  }

  /** Resumes a suspended context. Call from a user gesture (play). Never rejects. */
  async resume(): Promise<void> {
    if (this.closed || this.ctx.state !== 'suspended') return;
    try {
      await this.ctx.resume();
    } catch {
      // The policy refused the resume; playback stays silent until the next gesture.
    }
  }

  /** Suspends the context (no clock advance, no audio). Never rejects. */
  async suspend(): Promise<void> {
    if (this.closed || this.ctx.state !== 'running') return;
    try {
      await this.ctx.suspend();
    } catch {
      // Already closed or suspended.
    }
  }

  /** Disconnects every node and closes the context. Safe to call more than once. */
  close(): void {
    if (this.closed) return;
    this.closed = true;
    for (const entry of this.media.values()) safeDisconnect(entry.node);
    for (const chain of this.chains.values()) {
      for (const node of chainNodes(chain)) safeDisconnect(node);
    }
    for (const bus of this.tracks.values()) {
      safeDisconnect(bus.gain);
      safeDisconnect(bus.meter);
    }
    safeDisconnect(this.master);
    safeDisconnect(this.masterMeter);
    this.media.clear();
    this.chains.clear();
    this.tracks.clear();
    try {
      this.ctx.close().catch(() => {});
    } catch {
      // close() threw synchronously: nothing else to release.
    }
  }

  /** Points the chain input at the mono stage or straight at the splitter. Only called on change. */
  private routeMono(chain: SourceChain, mono: boolean): void {
    safeDisconnect(chain.clipGain);
    chain.clipGain.connect(mono ? chain.monoDown : chain.splitter);
    chain.mono = mono;
  }

  /** Returns the chain for `key`, creating it and wiring the media source when known. */
  private chainFor(key: string, mediaId: string): SourceChain {
    const existing = this.chains.get(key);
    if (existing) {
      if (existing.mediaId !== mediaId) {
        const old = this.media.get(existing.mediaId);
        if (old) {
          try {
            old.node.disconnect(existing.clipGain);
          } catch {
            // Not connected: nothing to undo.
          }
        }
        existing.mediaId = mediaId;
        const next = this.media.get(mediaId);
        if (next) next.node.connect(existing.clipGain);
      }
      return existing;
    }

    const ctx = this.ctx;
    const clipGain = ctx.createGain();
    clipGain.channelCount = 2;
    clipGain.channelCountMode = 'explicit';
    clipGain.channelInterpretation = 'speakers';
    clipGain.gain.setValueAtTime(0, ctx.currentTime);

    const monoDown = ctx.createGain();
    monoDown.channelCount = 1;
    monoDown.channelCountMode = 'explicit';
    monoDown.channelInterpretation = 'speakers';
    const monoUp = ctx.createGain();
    monoUp.channelCount = 2;
    monoUp.channelCountMode = 'explicit';
    monoUp.channelInterpretation = 'speakers';
    monoDown.connect(monoUp);

    const splitter = ctx.createChannelSplitter(2);
    const panL = ctx.createGain();
    const panR = ctx.createGain();
    panL.channelCount = 1;
    panL.channelCountMode = 'explicit';
    panR.channelCount = 1;
    panR.channelCountMode = 'explicit';
    panL.gain.setValueAtTime(1, ctx.currentTime);
    panR.gain.setValueAtTime(1, ctx.currentTime);
    const merger = ctx.createChannelMerger(2);

    monoUp.connect(splitter);
    splitter.connect(panL, 0);
    splitter.connect(panR, 1);
    panL.connect(merger, 0, 0);
    panR.connect(merger, 0, 1);

    const chain: SourceChain = {
      mediaId,
      clipGain,
      monoDown,
      monoUp,
      splitter,
      panL,
      panR,
      merger,
      trackIndex: -1,
      mono: false,
    };
    clipGain.connect(splitter);
    this.chains.set(key, chain);
    const source = this.media.get(mediaId);
    if (source) source.node.connect(clipGain);
    return chain;
  }

  /** Returns the bus for a track index, creating gain and meter on first use. */
  private trackBus(trackIndex: number): TrackBus {
    const existing = this.tracks.get(trackIndex);
    if (existing) return existing;
    const gain = this.ctx.createGain();
    const meter = this.ctx.createAnalyser();
    meter.fftSize = METER_FFT;
    gain.connect(meter);
    meter.connect(this.master);
    const bus: TrackBus = { gain, meter, buffer: new Float32Array(METER_FFT) };
    this.tracks.set(trackIndex, bus);
    return bus;
  }
}
