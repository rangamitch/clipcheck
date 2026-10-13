import { afterEach, beforeEach, describe, expect, it } from 'vitest';
import { AudioGraph } from './audioGraph';
import { panGains, type GainPoint } from './scheduler';

// ---------------------------------------------------------------------------------------------
// Hand-written Web Audio fakes. They record wiring and scheduling calls; nothing is rendered.
// Creation order inside one chain (after the master gain and master meter made by the
// constructor): clipGain, monoDown, monoUp, panL, panR, then (for a new track) the bus gain.
// ---------------------------------------------------------------------------------------------

interface ParamCall {
  op: string;
  args: number[];
}

class FakeParam {
  value = 0;
  calls: ParamCall[] = [];
  /** Present only when the fake context "supports" cancelAndHoldAtTime. */
  cancelAndHoldAtTime?: (t: number) => void;
  /** When set, `cancelAndHoldAtTime` throws (to exercise the fallback). */
  holdThrows = false;

  constructor(hold: boolean) {
    if (hold) {
      this.cancelAndHoldAtTime = (t: number): void => {
        if (this.holdThrows) throw new Error('hold refused');
        this.calls.push({ op: 'cancelAndHoldAtTime', args: [t] });
      };
    }
  }
  setValueAtTime(v: number, t: number): void {
    this.calls.push({ op: 'setValueAtTime', args: [v, t] });
  }
  linearRampToValueAtTime(v: number, t: number): void {
    this.calls.push({ op: 'linearRampToValueAtTime', args: [v, t] });
  }
  setTargetAtTime(v: number, t: number, tc: number): void {
    this.calls.push({ op: 'setTargetAtTime', args: [v, t, tc] });
  }
  cancelScheduledValues(t: number): void {
    this.calls.push({ op: 'cancelScheduledValues', args: [t] });
  }
  ops(op: string): ParamCall[] {
    return this.calls.filter((c) => c.op === op);
  }
  last(op: string): ParamCall | undefined {
    return this.ops(op).at(-1);
  }
}

interface Edge {
  to: unknown;
  out?: number;
  inp?: number;
}

class FakeNode {
  edges: Edge[] = [];
  disconnectCount = 0;
  disconnectArgs: unknown[] = [];
  channelCount = 2;
  channelCountMode = 'max';
  channelInterpretation = 'speakers';

  connect(to: unknown, out?: number, inp?: number): void {
    this.edges.push({ to, out, inp });
  }
  disconnect(to?: unknown): void {
    this.disconnectCount++;
    this.disconnectArgs.push(to);
    this.edges = to === undefined ? [] : this.edges.filter((e) => e.to !== to);
  }
  connectsTo(to: unknown): boolean {
    return this.edges.some((e) => e.to === to);
  }
}

class FakeGain extends FakeNode {
  gain: FakeParam;
  constructor(hold: boolean) {
    super();
    this.gain = new FakeParam(hold);
  }
}

class FakeAnalyser extends FakeNode {
  fftSize = 2048;
  /** Samples returned by getFloatTimeDomainData (repeated to fill the buffer). */
  samples: number[] = [];
  getFloatTimeDomainData(buf: Float32Array): void {
    for (let i = 0; i < buf.length; i++) buf[i] = this.samples[i % Math.max(1, this.samples.length)] ?? 0;
  }
}

class FakeSplitter extends FakeNode {
  constructor(readonly outputs: number) {
    super();
  }
}

class FakeMerger extends FakeNode {
  constructor(readonly inputs: number) {
    super();
  }
}

class FakeSource extends FakeNode {
  constructor(readonly element: unknown) {
    super();
  }
}

class FakeAudioContext {
  currentTime = 0;
  state: 'running' | 'suspended' | 'closed' = 'running';
  destination = new FakeNode();
  /** Whether new gain params expose cancelAndHoldAtTime. */
  holdSupport = true;
  created = {
    gains: [] as FakeGain[],
    analysers: [] as FakeAnalyser[],
    splitters: [] as FakeSplitter[],
    mergers: [] as FakeMerger[],
    sources: [] as FakeSource[],
  };
  closeCalls = 0;
  resumeCalls = 0;
  suspendCalls = 0;
  closeImpl: (() => Promise<void>) | null = null;

  createGain(): FakeGain {
    const g = new FakeGain(this.holdSupport);
    this.created.gains.push(g);
    return g;
  }
  createAnalyser(): FakeAnalyser {
    const a = new FakeAnalyser();
    this.created.analysers.push(a);
    return a;
  }
  createChannelSplitter(n: number): FakeSplitter {
    const s = new FakeSplitter(n);
    this.created.splitters.push(s);
    return s;
  }
  createChannelMerger(n: number): FakeMerger {
    const m = new FakeMerger(n);
    this.created.mergers.push(m);
    return m;
  }
  createMediaElementSource(el: unknown): FakeSource {
    const s = new FakeSource(el);
    this.created.sources.push(s);
    return s;
  }
  resume(): Promise<void> {
    this.resumeCalls++;
    this.state = 'running';
    return Promise.resolve();
  }
  suspend(): Promise<void> {
    this.suspendCalls++;
    this.state = 'suspended';
    return Promise.resolve();
  }
  close(): Promise<void> {
    this.closeCalls++;
    this.state = 'closed';
    return this.closeImpl ? this.closeImpl() : Promise.resolve();
  }
}

/** The context created by the most recent `new AudioGraph()`. */
let lastCtx: FakeAudioContext | undefined;

class CtorSpy extends FakeAudioContext {
  constructor() {
    super();
    lastCtx = this;
  }
}

const g = globalThis as unknown as Record<string, unknown>;

/** Clip gain ramp guard, seconds (mirrors the module constant). */
const CLICK_S = 0.005;
/** Fade-to-silence length, seconds (mirrors the module constant). */
const SILENCE_S = 0.01;

function el(): HTMLMediaElement {
  return {} as unknown as HTMLMediaElement;
}

function ctx(): FakeAudioContext {
  if (lastCtx === undefined) throw new Error('no context yet');
  return lastCtx;
}

/** Index access that fails the test with a clear message instead of returning undefined. */
function at<T>(list: readonly T[], i: number): T {
  const v = list[i];
  if (v === undefined) throw new Error(`no item at ${i}`);
  return v;
}

const ramp = (...points: GainPoint[]): GainPoint[] => points;

/** Chain nodes of the first clip, by index into `created` (see the header comment). */
function firstChain(): {
  clip: FakeGain;
  monoDown: FakeGain;
  monoUp: FakeGain;
  panL: FakeGain;
  panR: FakeGain;
  splitter: FakeSplitter;
  merger: FakeMerger;
} {
  const gains = ctx().created.gains;
  return {
    clip: at(gains, 1),
    monoDown: at(gains, 2),
    monoUp: at(gains, 3),
    panL: at(gains, 4),
    panR: at(gains, 5),
    splitter: at(ctx().created.splitters, 0),
    merger: at(ctx().created.mergers, 0),
  };
}

/** Gains created per chain (clip, monoDown, monoUp, panL, panR). */
const CHAIN_GAINS = 5;

beforeEach(() => {
  lastCtx = undefined;
  g.AudioContext = CtorSpy;
});

afterEach(() => {
  delete g.AudioContext;
  lastCtx = undefined;
});

// ---------------------------------------------------------------------------------------------
describe('construction', () => {
  it('creates one audio context and exposes it', () => {
    const graph = new AudioGraph();
    expect(lastCtx).toBeDefined();
    expect(graph.ctx).toBe(ctx());
  });

  it('creates the master gain and a master meter with a 1024 sample window', () => {
    new AudioGraph();
    expect(ctx().created.gains.length).toBe(1);
    expect(ctx().created.analysers.length).toBe(1);
    expect(at(ctx().created.analysers, 0).fftSize).toBe(1024);
  });

  it('routes master gain to the master meter and the meter to the destination', () => {
    new AudioGraph();
    const master = at(ctx().created.gains, 0);
    const meter = at(ctx().created.analysers, 0);
    expect(master.connectsTo(meter)).toBe(true);
    expect(meter.connectsTo(ctx().destination)).toBe(true);
  });

  it('starts with no media sources and no chains', () => {
    new AudioGraph();
    expect(ctx().created.sources).toEqual([]);
    expect(ctx().created.splitters).toEqual([]);
  });
});

// ---------------------------------------------------------------------------------------------
describe('attach', () => {
  it('routes the default stream (key m1#0) and an extra stream (m1#1) of one media separately', () => {
    // The engine keys every audio element `<mediaId>#<stream>`, stream 0 included.
    const graph = new AudioGraph();
    graph.attach('m1#0', el());
    graph.attach('m1#1', el());
    graph.setSourceParams('c0', 'm1#0', 0, ramp({ offsetSec: 0, gain: 1 }), 0, false, 0);
    graph.setSourceParams('c1', 'm1#1', 0, ramp({ offsetSec: 0, gain: 1 }), 0, false, 0);
    expect(ctx().created.sources.length).toBe(2);
    const gains = ctx().created.gains;
    const clip0 = at(gains, 1);
    const clip1 = at(gains, 2 + CHAIN_GAINS); // +1: the track bus gain made by the first chain
    expect(at(ctx().created.sources, 0).connectsTo(clip0)).toBe(true);
    expect(at(ctx().created.sources, 0).connectsTo(clip1)).toBe(false);
    expect(at(ctx().created.sources, 1).connectsTo(clip1)).toBe(true);
  });

  it('creates one source node for a media element', () => {
    const graph = new AudioGraph();
    graph.attach('m1', el());
    expect(ctx().created.sources.length).toBe(1);
  });

  it('attaching the same element again creates no second source node', () => {
    const graph = new AudioGraph();
    const v = el();
    graph.attach('m1', v);
    graph.attach('m1', v);
    expect(ctx().created.sources.length).toBe(1);
  });

  it('attaching the same element to the same media id is a no-op', () => {
    const graph = new AudioGraph();
    const v = el();
    graph.attach('m1', v);
    const src = at(ctx().created.sources, 0);
    graph.attach('m1', v);
    expect(src.disconnectCount).toBe(0);
  });

  it('the same element can serve a second media id without a new source node', () => {
    const graph = new AudioGraph();
    const v = el();
    graph.attach('m1', v);
    graph.attach('m2', v);
    expect(ctx().created.sources.length).toBe(1);
  });

  it('a different element for the same media id creates a new source node', () => {
    const graph = new AudioGraph();
    graph.attach('m1', el());
    graph.attach('m1', el());
    expect(ctx().created.sources.length).toBe(2);
  });

  it('replacing the element disconnects the old source node', () => {
    const graph = new AudioGraph();
    graph.attach('m1', el());
    const old = at(ctx().created.sources, 0);
    graph.attach('m1', el());
    expect(old.disconnectCount).toBeGreaterThanOrEqual(1);
  });

  it('a chain that uses the media is moved to the new source node', () => {
    const graph = new AudioGraph();
    graph.attach('m1', el());
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    const clip = firstChain().clip;
    graph.attach('m1', el());
    expect(at(ctx().created.sources, 1).connectsTo(clip)).toBe(true);
  });

  it('a chain created before its media was attached is connected when the media arrives', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    graph.attach('m1', el());
    expect(at(ctx().created.sources, 0).connectsTo(firstChain().clip)).toBe(true);
  });

  it('attaching a media id does not touch chains that use other media', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c2', 'm2', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    graph.attach('m1', el());
    expect(at(ctx().created.sources, 0).edges.length).toBe(0);
  });

  it('attach after close is ignored', () => {
    const graph = new AudioGraph();
    graph.close();
    graph.attach('m1', el());
    expect(ctx().created.sources.length).toBe(0);
  });
});

// ---------------------------------------------------------------------------------------------
describe('detach', () => {
  it('disconnects the source node of the media', () => {
    const graph = new AudioGraph();
    graph.attach('m1', el());
    const src = at(ctx().created.sources, 0);
    graph.detach('m1');
    expect(src.disconnectCount).toBeGreaterThanOrEqual(1);
  });

  it('a chain set up after detach is not connected to the detached source', () => {
    const graph = new AudioGraph();
    graph.attach('m1', el());
    graph.detach('m1');
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    expect(at(ctx().created.sources, 0).connectsTo(firstChain().clip)).toBe(false);
  });

  it('attaching the same element again after detach reuses its source node', () => {
    const graph = new AudioGraph();
    const v = el();
    graph.attach('m1', v);
    graph.detach('m1');
    graph.attach('m1', v);
    expect(ctx().created.sources.length).toBe(1);
  });

  it('detach of an unknown media id is a no-op', () => {
    const graph = new AudioGraph();
    expect(() => graph.detach('nope')).not.toThrow();
  });

  it('detach after close is ignored', () => {
    const graph = new AudioGraph();
    graph.attach('m1', el());
    graph.close();
    const src = at(ctx().created.sources, 0);
    const before = src.disconnectCount;
    graph.detach('m1');
    expect(src.disconnectCount).toBe(before);
  });
});

// ---------------------------------------------------------------------------------------------
describe('chain wiring', () => {
  it('the clip gain feeds the splitter directly when not mono', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    const c = firstChain();
    expect(c.clip.connectsTo(c.splitter)).toBe(true);
    expect(c.clip.connectsTo(c.monoDown)).toBe(false);
  });

  it('the mono stage is a downmix into the upmix, which feeds the splitter', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    const c = firstChain();
    expect(c.monoDown.connectsTo(c.monoUp)).toBe(true);
    expect(c.monoUp.connectsTo(c.splitter)).toBe(true);
  });

  it('the splitter sends left to the left pan gain and right to the right pan gain', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    const c = firstChain();
    expect(c.splitter.edges).toEqual(
      expect.arrayContaining([
        { to: c.panL, out: 0, inp: undefined },
        { to: c.panR, out: 1, inp: undefined },
      ]),
    );
  });

  it('both pan gains feed the merger, one per channel', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    const c = firstChain();
    expect(c.panL.edges).toEqual([{ to: c.merger, out: 0, inp: 0 }]);
    expect(c.panR.edges).toEqual([{ to: c.merger, out: 0, inp: 1 }]);
  });

  it('the clip gain is stereo with an explicit channel count', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    const clip = firstChain().clip;
    expect(clip.channelCount).toBe(2);
    expect(clip.channelCountMode).toBe('explicit');
  });

  it('the pan gains are mono (one channel each)', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    const c = firstChain();
    expect(c.panL.channelCount).toBe(1);
    expect(c.panR.channelCount).toBe(1);
  });

  it('a splitter and merger are created with two channels each', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    expect(firstChain().splitter.outputs).toBe(2);
    expect(firstChain().merger.inputs).toBe(2);
  });

  it('a chain creates exactly five gains and one splitter and one merger', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 0);
    // master + five chain gains + one track bus gain
    expect(ctx().created.gains.length).toBe(1 + CHAIN_GAINS + 1);
    expect(ctx().created.splitters.length).toBe(1);
    expect(ctx().created.mergers.length).toBe(1);
  });

  it('a chain is created once per key', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 0.5, offsetSec: 0 }), 0, false, 0);
    expect(ctx().created.splitters.length).toBe(1);
  });
});

// ---------------------------------------------------------------------------------------------
describe('setSourceParams: gain curve', () => {
  it('sets the first point with a short click-guard ramp from the held value', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 0.5, offsetSec: 0 }), 0, false, 1);
    expect(firstChain().clip.gain.last('linearRampToValueAtTime')?.args).toEqual([0.5, 1 + CLICK_S]);
  });

  it('holds the current value at the start time when cancelAndHoldAtTime exists', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 0.5, offsetSec: 0 }), 0, false, 2);
    expect(firstChain().clip.gain.last('cancelAndHoldAtTime')?.args).toEqual([2]);
  });

  it('falls back to cancelScheduledValues and pinning the value when cancelAndHoldAtTime is missing', () => {
    const graph = new AudioGraph();
    // Gain params made from here on have no cancelAndHoldAtTime.
    ctx().holdSupport = false;
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 0.5, offsetSec: 0 }), 0, false, 2);
    const p = firstChain().clip.gain;
    expect(p.last('cancelScheduledValues')?.args).toEqual([2]);
    expect(p.calls.some((c) => c.op === 'setValueAtTime' && c.args[1] === 2)).toBe(true);
  });

  it('falls back when cancelAndHoldAtTime throws', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 0);
    const p = firstChain().clip.gain;
    p.holdThrows = true;
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 0.5, offsetSec: 0 }), 0, false, 3);
    expect(p.last('cancelScheduledValues')?.args).toEqual([3]);
  });

  it('later ramp points are scheduled relative to the start time', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 0, offsetSec: 0 }, { gain: 1, offsetSec: 0.5 }), 0, false, 2);
    expect(firstChain().clip.gain.ops('linearRampToValueAtTime').at(-1)?.args).toEqual([1, 2.5]);
  });

  it('a ramp point in the past is placed at the start time', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 0, offsetSec: 0 }, { gain: 1, offsetSec: -3 }), 0, false, 2);
    expect(firstChain().clip.gain.ops('linearRampToValueAtTime').at(-1)?.args).toEqual([1, 2]);
  });

  it('an empty ramp silences the clip gain with the click guard', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 4);
    expect(firstChain().clip.gain.last('linearRampToValueAtTime')?.args).toEqual([0, 4 + CLICK_S]);
  });

  it('a gain above the 4x ceiling is clamped to 4', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 10, offsetSec: 0 }), 0, false, 0);
    expect(firstChain().clip.gain.last('linearRampToValueAtTime')?.args[0]).toBe(4);
  });

  it('a negative gain is clamped to 0', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: -2, offsetSec: 0 }), 0, false, 0);
    expect(firstChain().clip.gain.last('linearRampToValueAtTime')?.args[0]).toBe(0);
  });

  it('a NaN gain is clamped to 0', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: NaN, offsetSec: 0 }), 0, false, 0);
    expect(firstChain().clip.gain.last('linearRampToValueAtTime')?.args[0]).toBe(0);
  });

  it('a later ramp value above the ceiling is also clamped', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }, { gain: 99, offsetSec: 1 }), 0, false, 0);
    expect(firstChain().clip.gain.last('linearRampToValueAtTime')?.args[0]).toBe(4);
  });

  it('a start time before the context clock is moved up to the clock', () => {
    const graph = new AudioGraph();
    ctx().currentTime = 5;
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 2);
    expect(firstChain().clip.gain.last('cancelAndHoldAtTime')?.args).toEqual([5]);
  });
});

// ---------------------------------------------------------------------------------------------
describe('setSourceParams: pan, mono and tracks', () => {
  it('pan gains come from the shared constant-power law', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0.25, false, 0);
    const want = panGains(0.25);
    expect(firstChain().panL.gain.last('setValueAtTime')?.args[0]).toBeCloseTo(want.l, 9);
    expect(firstChain().panR.gain.last('setValueAtTime')?.args[0]).toBeCloseTo(want.r, 9);
  });

  it('hard left silences the right channel', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), -1, false, 0);
    expect(firstChain().panR.gain.last('setValueAtTime')?.args[0]).toBeCloseTo(panGains(-1).r, 9);
    expect(panGains(-1).r).toBeCloseTo(0, 9);
  });

  it('pan gains are set at the start time', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 7);
    expect(firstChain().panL.gain.last('setValueAtTime')?.args[1]).toBe(7);
  });

  it('enabling mono reroutes the clip gain through the mono stage', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, true, 0);
    const c = firstChain();
    expect(c.clip.connectsTo(c.monoDown)).toBe(true);
    expect(c.clip.connectsTo(c.splitter)).toBe(false);
  });

  it('disabling mono sends the clip gain straight to the splitter again', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, true, 0);
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    const c = firstChain();
    expect(c.clip.connectsTo(c.splitter)).toBe(true);
    expect(c.clip.connectsTo(c.monoDown)).toBe(false);
  });

  it('a mono flag that does not change does not reroute the clip gain', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    const before = firstChain().clip.disconnectCount;
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 1);
    expect(firstChain().clip.disconnectCount).toBe(before);
  });

  it('the first use of a track connects the merger to a new bus gain', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    expect(firstChain().merger.connectsTo(at(ctx().created.gains, 1 + CHAIN_GAINS))).toBe(true);
  });

  it('repeating the same track index does not reconnect the merger', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 1);
    expect(firstChain().merger.disconnectCount).toBe(0);
  });

  it('moving to another track disconnects the merger and connects the new bus', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    graph.setSourceParams('c1', 'm1', 1, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    const merger = firstChain().merger;
    expect(merger.disconnectCount).toBe(1);
    expect(merger.connectsTo(ctx().created.gains.at(-1))).toBe(true);
  });

  it('two sources on the same track share one bus', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 0);
    graph.setSourceParams('c2', 'm1', 0, [], 0, false, 0);
    // master, two chains, one bus gain
    expect(ctx().created.gains.length).toBe(1 + 2 * CHAIN_GAINS + 1);
    expect(ctx().created.analysers.length).toBe(2); // master meter + one bus meter
  });

  it('sources on two tracks create two buses', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 0);
    graph.setSourceParams('c2', 'm1', 1, [], 0, false, 0);
    expect(ctx().created.analysers.length).toBe(3);
  });

  it('a key that switches media moves its connection to the new source', () => {
    const graph = new AudioGraph();
    graph.attach('m1', el());
    graph.attach('m2', el());
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    const clip = firstChain().clip;
    graph.setSourceParams('c1', 'm2', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    expect(at(ctx().created.sources, 0).disconnectArgs).toContain(clip);
    expect(at(ctx().created.sources, 1).connectsTo(clip)).toBe(true);
  });

  it('a key that keeps its media does not disconnect the source', () => {
    const graph = new AudioGraph();
    graph.attach('m1', el());
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    const src = at(ctx().created.sources, 0);
    const before = src.disconnectCount;
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    expect(src.disconnectCount).toBe(before);
  });

  it('two keys on the same media feed the same source node', () => {
    const graph = new AudioGraph();
    graph.attach('m1', el());
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 0);
    graph.setSourceParams('c2', 'm1', 0, [], 0, false, 0);
    expect(at(ctx().created.sources, 0).edges.length).toBe(2);
  });

  it('setSourceParams after close is ignored', () => {
    const graph = new AudioGraph();
    graph.close();
    const before = ctx().created.gains.length;
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 0);
    expect(ctx().created.gains.length).toBe(before);
  });
});

// ---------------------------------------------------------------------------------------------
describe('silenceSource and removeSource', () => {
  it('silenceSource holds the gain now and ramps to zero over 10 ms', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    ctx().currentTime = 3;
    graph.silenceSource('c1');
    const p = firstChain().clip.gain;
    expect(p.last('cancelAndHoldAtTime')?.args).toEqual([3]);
    expect(p.last('linearRampToValueAtTime')?.args).toEqual([0, 3 + SILENCE_S]);
  });

  it('silenceSource of an unknown key is ignored', () => {
    const graph = new AudioGraph();
    expect(() => graph.silenceSource('nope')).not.toThrow();
  });

  it('silenceSource after close is ignored', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    graph.close();
    const before = firstChain().clip.gain.calls.length;
    graph.silenceSource('c1');
    expect(firstChain().clip.gain.calls.length).toBe(before);
  });

  it('removeSource disconnects the clip gain from its media source', () => {
    const graph = new AudioGraph();
    graph.attach('m1', el());
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    const clip = firstChain().clip;
    graph.removeSource('c1');
    expect(at(ctx().created.sources, 0).disconnectArgs).toContain(clip);
  });

  it('removeSource disconnects every node of the chain', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    graph.removeSource('c1');
    const c = firstChain();
    for (const n of [c.clip, c.monoDown, c.monoUp, c.panL, c.panR]) expect(n.disconnectCount).toBeGreaterThanOrEqual(1);
    expect(c.splitter.disconnectCount).toBeGreaterThanOrEqual(1);
    expect(c.merger.disconnectCount).toBeGreaterThanOrEqual(1);
  });

  it('removeSource cancels the scheduled gain at the current time', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, ramp({ gain: 1, offsetSec: 0 }), 0, false, 0);
    ctx().currentTime = 2;
    graph.removeSource('c1');
    expect(firstChain().clip.gain.last('cancelScheduledValues')?.args).toEqual([2]);
  });

  it('a removed key builds a fresh chain when it is set again', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 0);
    graph.removeSource('c1');
    const before = ctx().created.gains.length;
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 0);
    expect(ctx().created.gains.length - before).toBe(CHAIN_GAINS);
  });

  it('removeSource of an unknown key is ignored', () => {
    const graph = new AudioGraph();
    expect(() => graph.removeSource('nope')).not.toThrow();
  });

  it('removeSource after close is ignored', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 0);
    graph.close();
    expect(() => graph.removeSource('c1')).not.toThrow();
  });
});

// ---------------------------------------------------------------------------------------------
describe('setMaster', () => {
  it('moves the master gain to the requested linear value', () => {
    const graph = new AudioGraph();
    graph.setMaster(0.5);
    expect(at(ctx().created.gains, 0).gain.last('setTargetAtTime')?.args).toEqual([0.5, 0, 0.01]);
  });

  it('caps the master gain at 4 (about +12 dB)', () => {
    const graph = new AudioGraph();
    graph.setMaster(10);
    expect(at(ctx().created.gains, 0).gain.last('setTargetAtTime')?.args[0]).toBe(4);
  });

  it('a negative master gain becomes silence', () => {
    const graph = new AudioGraph();
    graph.setMaster(-1);
    expect(at(ctx().created.gains, 0).gain.last('setTargetAtTime')?.args[0]).toBe(0);
  });

  it('a NaN master gain becomes silence', () => {
    const graph = new AudioGraph();
    graph.setMaster(NaN);
    expect(at(ctx().created.gains, 0).gain.last('setTargetAtTime')?.args[0]).toBe(0);
  });

  it('uses a 10 ms time constant', () => {
    const graph = new AudioGraph();
    graph.setMaster(1);
    expect(at(ctx().created.gains, 0).gain.last('setTargetAtTime')?.args[2]).toBe(0.01);
  });

  it('setMaster after close is ignored', () => {
    const graph = new AudioGraph();
    graph.close();
    graph.setMaster(0.5);
    expect(at(ctx().created.gains, 0).gain.calls.length).toBe(0);
  });
});

// ---------------------------------------------------------------------------------------------
describe('levels', () => {
  it('reads -120 dBFS from the master meter when nothing plays', () => {
    const graph = new AudioGraph();
    expect(graph.levels()).toEqual({ master: -120, tracks: [] });
  });

  it('reports the master peak in dBFS', () => {
    const graph = new AudioGraph();
    at(ctx().created.analysers, 0).samples = [0.5];
    expect(graph.levels().master).toBeCloseTo(-6.0206, 3);
  });

  it('uses the absolute peak of the samples', () => {
    const graph = new AudioGraph();
    at(ctx().created.analysers, 0).samples = [-0.25, 0.1];
    expect(graph.levels().master).toBeCloseTo(-12.0412, 3);
  });

  it('full scale reads 0 dBFS', () => {
    const graph = new AudioGraph();
    at(ctx().created.analysers, 0).samples = [1];
    expect(graph.levels().master).toBeCloseTo(0, 9);
  });

  it('a silent buffer reads -120 dBFS', () => {
    const graph = new AudioGraph();
    at(ctx().created.analysers, 0).samples = [0, 0];
    expect(graph.levels().master).toBe(-120);
  });

  it('a very quiet signal is floored at -120 dBFS', () => {
    const graph = new AudioGraph();
    at(ctx().created.analysers, 0).samples = [1e-9];
    expect(graph.levels().master).toBe(-120);
  });

  it('tracks below the highest used index read -120 when they have no bus', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 2, [], 0, false, 0);
    const levels = graph.levels();
    expect(levels.tracks.length).toBe(3);
    expect(levels.tracks[0]).toBe(-120);
    expect(levels.tracks[1]).toBe(-120);
  });

  it('reads the peak of a track bus meter at its index', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 2, [], 0, false, 0);
    at(ctx().created.analysers, 1).samples = [0.5];
    expect(graph.levels().tracks[2]).toBeCloseTo(-6.0206, 3);
  });

  it('a bus with no signal reads -120 dBFS', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 0);
    expect(graph.levels().tracks).toEqual([-120]);
  });

  it('after close the levels are idle', () => {
    const graph = new AudioGraph();
    graph.close();
    expect(graph.levels()).toEqual({ master: -120, tracks: [] });
  });
});

// ---------------------------------------------------------------------------------------------
describe('resume and suspend', () => {
  it('resume starts a suspended context', async () => {
    const graph = new AudioGraph();
    ctx().state = 'suspended';
    await graph.resume();
    expect(ctx().resumeCalls).toBe(1);
  });

  it('resume does nothing when the context is already running', async () => {
    const graph = new AudioGraph();
    await graph.resume();
    expect(ctx().resumeCalls).toBe(0);
  });

  it('a refused resume is swallowed', async () => {
    const graph = new AudioGraph();
    ctx().state = 'suspended';
    ctx().resume = () => Promise.reject(new Error('autoplay policy'));
    await expect(graph.resume()).resolves.toBeUndefined();
  });

  it('resume after close does nothing', async () => {
    const graph = new AudioGraph();
    ctx().state = 'suspended';
    graph.close();
    await graph.resume();
    expect(ctx().resumeCalls).toBe(0);
  });

  it('suspend suspends a running context', async () => {
    const graph = new AudioGraph();
    await graph.suspend();
    expect(ctx().suspendCalls).toBe(1);
  });

  it('suspend does nothing when the context is not running', async () => {
    const graph = new AudioGraph();
    ctx().state = 'suspended';
    await graph.suspend();
    expect(ctx().suspendCalls).toBe(0);
  });

  it('a failing suspend is swallowed', async () => {
    const graph = new AudioGraph();
    ctx().suspend = () => Promise.reject(new Error('already closed'));
    await expect(graph.suspend()).resolves.toBeUndefined();
  });
});

// ---------------------------------------------------------------------------------------------
describe('close', () => {
  it('closes the audio context once', () => {
    const graph = new AudioGraph();
    graph.close();
    expect(ctx().closeCalls).toBe(1);
  });

  it('a second close does nothing', () => {
    const graph = new AudioGraph();
    graph.close();
    graph.close();
    expect(ctx().closeCalls).toBe(1);
  });

  it('disconnects every media source', () => {
    const graph = new AudioGraph();
    graph.attach('m1', el());
    graph.attach('m2', el());
    graph.close();
    for (const s of ctx().created.sources) expect(s.disconnectCount).toBeGreaterThanOrEqual(1);
  });

  it('disconnects every node of every chain', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 0);
    graph.close();
    const c = firstChain();
    for (const n of [c.clip, c.monoDown, c.monoUp, c.panL, c.panR]) expect(n.disconnectCount).toBeGreaterThanOrEqual(1);
    expect(c.splitter.disconnectCount).toBeGreaterThanOrEqual(1);
    expect(c.merger.disconnectCount).toBeGreaterThanOrEqual(1);
  });

  it('disconnects the track bus, the master chain and the master meter', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 0);
    graph.close();
    expect(at(ctx().created.gains, 1 + CHAIN_GAINS).disconnectCount).toBeGreaterThanOrEqual(1);
    expect(at(ctx().created.analysers, 1).disconnectCount).toBeGreaterThanOrEqual(1);
    expect(at(ctx().created.gains, 0).disconnectCount).toBeGreaterThanOrEqual(1);
    expect(at(ctx().created.analysers, 0).disconnectCount).toBeGreaterThanOrEqual(1);
  });

  it('a close that rejects is absorbed', async () => {
    const graph = new AudioGraph();
    ctx().closeImpl = () => Promise.reject(new Error('already closed'));
    expect(() => graph.close()).not.toThrow();
    await Promise.resolve();
  });

  it('a close that throws synchronously is absorbed', () => {
    const graph = new AudioGraph();
    ctx().close = () => {
      throw new Error('sync boom');
    };
    expect(() => graph.close()).not.toThrow();
  });

  it('after close attach is ignored', () => {
    const graph = new AudioGraph();
    graph.close();
    expect(() => graph.attach('m1', el())).not.toThrow();
    expect(ctx().created.sources.length).toBe(0);
  });

  it('after close levels report silence', () => {
    const graph = new AudioGraph();
    graph.setSourceParams('c1', 'm1', 0, [], 0, false, 0);
    graph.close();
    expect(graph.levels().master).toBe(-120);
  });

  it('after close silenceSource, setMaster and removeSource are ignored', () => {
    const graph = new AudioGraph();
    graph.close();
    expect(() => {
      graph.silenceSource('c1');
      graph.setMaster(1);
      graph.removeSource('c1');
      graph.detach('m1');
    }).not.toThrow();
  });
});

// ---------------------------------------------------------------------------------------------
describe('sharing', () => {
  it('a second AudioGraph gets its own context', () => {
    new AudioGraph();
    const first = ctx();
    new AudioGraph();
    expect(ctx()).not.toBe(first);
  });

  it('closing one graph leaves another one usable', () => {
    const a = new AudioGraph();
    const aCtx = ctx();
    const b = new AudioGraph();
    a.close();
    expect(aCtx.closeCalls).toBe(1);
    b.setMaster(0.25);
    expect(at(ctx().created.gains, 0).gain.last('setTargetAtTime')?.args[0]).toBe(0.25);
  });
});
