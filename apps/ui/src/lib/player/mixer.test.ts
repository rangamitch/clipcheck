import { describe, expect, it, vi } from 'vitest';
import { TrackMixer, syncAudioToVideo } from './mixer';
import { initTracks, setGainDb, soloTrack, toggleTrack, trackGain, type TrackState } from './model';

type FakeNode = {
  connect: ReturnType<typeof vi.fn>;
  disconnect: ReturnType<typeof vi.fn>;
};
type FakeGain = FakeNode & { gain: { value: number; setTargetAtTime: ReturnType<typeof vi.fn> } };
type FakeSource = FakeNode & { el: unknown };

function makeFakeContext(state: 'running' | 'suspended' = 'running') {
  const sources: FakeSource[] = [];
  const gains: FakeGain[] = [];
  const destination = { kind: 'destination' };
  const ctx = {
    state,
    currentTime: 0,
    destination,
    createMediaElementSource: vi.fn((el: unknown): FakeSource => {
      const s: FakeSource = { el, connect: vi.fn(), disconnect: vi.fn() };
      sources.push(s);
      return s;
    }),
    createGain: vi.fn((): FakeGain => {
      const g: FakeGain = {
        gain: { value: 1, setTargetAtTime: vi.fn() },
        connect: vi.fn(),
        disconnect: vi.fn(),
      };
      gains.push(g);
      return g;
    }),
    resume: vi.fn(() => Promise.resolve()),
    close: vi.fn(() => Promise.resolve()),
  };
  return { ctx, sources, gains, destination };
}

function makeEl(id: string): HTMLMediaElement {
  return { id } as unknown as HTMLMediaElement;
}

function makeMixer(ctxState: 'running' | 'suspended' = 'running') {
  const fake = makeFakeContext(ctxState);
  const createContext = vi.fn(() => fake.ctx as unknown as AudioContext);
  const mixer = new TrackMixer({ createContext });
  return { mixer, createContext, ...fake };
}

describe('TrackMixer attach', () => {
  it('does not create a context before attach', () => {
    const { createContext } = makeMixer();
    expect(createContext).not.toHaveBeenCalled();
  });

  it('creates one source and one gain per element and wires source -> gain -> destination', () => {
    const { mixer, createContext, ctx, sources, gains, destination } = makeMixer();
    const a = makeEl('a');
    const b = makeEl('b');
    mixer.attach([a, b]);

    expect(createContext).toHaveBeenCalledTimes(1);
    expect(ctx.createMediaElementSource).toHaveBeenCalledTimes(2);
    expect(ctx.createMediaElementSource).toHaveBeenNthCalledWith(1, a);
    expect(ctx.createMediaElementSource).toHaveBeenNthCalledWith(2, b);
    expect(ctx.createGain).toHaveBeenCalledTimes(2);
    expect(sources).toHaveLength(2);
    expect(gains).toHaveLength(2);

    sources.forEach((s, i) => {
      expect(s.connect).toHaveBeenCalledWith(gains[i]);
    });
    gains.forEach((g) => {
      expect(g.connect).toHaveBeenCalledWith(destination);
    });
  });

  it('attaching nothing creates no source or gain nodes', () => {
    const { mixer, ctx } = makeMixer();
    mixer.attach([]);
    expect(ctx.createMediaElementSource).not.toHaveBeenCalled();
    expect(ctx.createGain).not.toHaveBeenCalled();
  });

  it('attaching twice detaches the first set of nodes and closes its context', () => {
    const first = makeMixer();
    const { mixer, sources, gains, ctx } = first;
    mixer.attach([makeEl('a'), makeEl('b')]);
    const firstSources = [...sources];
    const firstGains = [...gains];

    mixer.attach([makeEl('c')]);

    firstSources.forEach((s) => expect(s.disconnect).toHaveBeenCalled());
    firstGains.forEach((g) => expect(g.disconnect).toHaveBeenCalled());
    expect(ctx.close).toHaveBeenCalled();
  });

  it('attached is false before attach, true after, and false after detach', () => {
    const { mixer } = makeMixer();
    expect(mixer.attached).toBe(false);
    mixer.attach([makeEl('a')]);
    expect(mixer.attached).toBe(true);
    mixer.detach();
    expect(mixer.attached).toBe(false);
  });
});

describe('TrackMixer apply', () => {
  function attached(n: number, ctxState: 'running' | 'suspended' = 'running') {
    const h = makeMixer(ctxState);
    h.mixer.attach(Array.from({ length: n }, (_, i) => makeEl(`e${i}`)));
    return h;
  }

  it('is a no-op before attach', () => {
    const { mixer, createContext } = makeMixer();
    expect(() => mixer.apply(initTracks(2), 1, false)).not.toThrow();
    expect(createContext).not.toHaveBeenCalled();
  });

  it('sets gain with setTargetAtTime (0.01 s) to trackGain for each element', () => {
    const { mixer, gains, ctx } = attached(2);
    const states = initTracks(2); // track 0 audible, track 1 silent
    mixer.apply(states, 1, false);

    expect(gains[0].gain.setTargetAtTime).toHaveBeenCalledWith(
      trackGain(states, 0, 1, false),
      expect.any(Number),
      0.01,
    );
    expect(gains[1].gain.setTargetAtTime).toHaveBeenCalledWith(0, expect.any(Number), 0.01);
    expect(ctx.createGain).toHaveBeenCalledTimes(2);
  });

  it('silences everything when muted', () => {
    const { mixer, gains } = attached(2);
    mixer.apply(initTracks(2), 1, true);
    for (const g of gains) {
      expect(g.gain.setTargetAtTime).toHaveBeenLastCalledWith(0, expect.any(Number), 0.01);
    }
  });

  it.each([
    ['audible track at volume 0.5', (s: TrackState[]) => s, 0.5, false, 0, 0.5],
    ['+6 dB gain on the audible track', (s: TrackState[]) => setGainDb(s, 0, 6), 1, false, 0, 1.995262],
    ['toggled-on second track', (s: TrackState[]) => toggleTrack(s, 1), 1, false, 1, 1],
    ['solo on track 1 silences track 0', (s: TrackState[]) => soloTrack(s, 1), 1, false, 0, 0],
    ['solo on track 1 plays track 1', (s: TrackState[]) => soloTrack(s, 1), 1, false, 1, 1],
    ['muted solo', (s: TrackState[]) => soloTrack(s, 1), 1, true, 1, 0],
  ])('%s: element %i gets gain %s', (_name, mk, volume, muted, idx, expected) => {
    const { mixer, gains } = attached(2);
    const states = mk(initTracks(2));
    mixer.apply(states, volume, muted);
    const last = gains[idx].gain.setTargetAtTime.mock.calls.at(-1);
    expect(last).toBeDefined();
    expect(last![0]).toBeCloseTo(expected, 4);
    expect(last![2]).toBe(0.01);
  });

  it('silences elements missing from the states array', () => {
    const { mixer, gains } = attached(3);
    mixer.apply(initTracks(1), 1, false);
    expect(gains[0].gain.setTargetAtTime).toHaveBeenLastCalledWith(1, expect.any(Number), 0.01);
    expect(gains[1].gain.setTargetAtTime.mock.calls.at(-1)?.[0]).toBe(0);
    expect(gains[2].gain.setTargetAtTime.mock.calls.at(-1)?.[0]).toBe(0);
  });
});

describe('TrackMixer resume', () => {
  it('calls ctx.resume only when the context is suspended', async () => {
    const running = makeMixer('running');
    running.mixer.attach([makeEl('a')]);
    await running.mixer.resume();
    expect(running.ctx.resume).not.toHaveBeenCalled();

    const suspended = makeMixer('suspended');
    suspended.mixer.attach([makeEl('a')]);
    await suspended.mixer.resume();
    expect(suspended.ctx.resume).toHaveBeenCalledTimes(1);
  });

  it('resolves without touching a context when unattached', async () => {
    const { mixer, createContext } = makeMixer('suspended');
    await expect(mixer.resume()).resolves.toBeUndefined();
    expect(createContext).not.toHaveBeenCalled();
  });
});

describe('TrackMixer detach', () => {
  it('is safe to call twice', () => {
    const { mixer, ctx } = makeMixer();
    mixer.attach([makeEl('a')]);
    mixer.detach();
    expect(() => mixer.detach()).not.toThrow();
    expect(ctx.close).toHaveBeenCalledTimes(1);
  });

  it('is safe before any attach', () => {
    const { mixer } = makeMixer();
    expect(() => mixer.detach()).not.toThrow();
  });

  it('tolerates a close() that rejects', async () => {
    const { mixer, ctx } = makeMixer();
    ctx.close.mockImplementation(() => Promise.reject(new Error('close failed')));
    mixer.attach([makeEl('a')]);
    expect(() => mixer.detach()).not.toThrow();
    // Give any unhandled rejection a chance to surface.
    await new Promise((r) => setTimeout(r, 0));
  });

  it('tolerates a disconnect() that throws', () => {
    const { mixer, sources, gains } = makeMixer();
    mixer.attach([makeEl('a'), makeEl('b')]);
    sources.forEach((s) =>
      s.disconnect.mockImplementation(() => {
        throw new Error('disconnect failed');
      }),
    );
    gains.forEach((g) =>
      g.disconnect.mockImplementation(() => {
        throw new Error('disconnect failed');
      }),
    );
    expect(() => mixer.detach()).not.toThrow();
  });
});

// ---------------------------------------------------------------------------
// syncAudioToVideo, with plain-object fakes
// ---------------------------------------------------------------------------

type FakeMedia = {
  paused: boolean;
  ended: boolean;
  currentTime: number;
  playbackRate: number;
  play: ReturnType<typeof vi.fn>;
  pause: ReturnType<typeof vi.fn>;
};

function fakeVideo(over: Partial<FakeMedia> = {}): FakeMedia {
  return {
    paused: false,
    ended: false,
    currentTime: 10,
    playbackRate: 1,
    play: vi.fn(() => Promise.resolve()),
    pause: vi.fn(),
    ...over,
  };
}

function fakeAudio(over: Partial<FakeMedia> = {}): FakeMedia {
  return fakeVideo({ paused: true, currentTime: 10, ...over });
}

function asEl(m: FakeMedia): HTMLMediaElement {
  return m as unknown as HTMLMediaElement;
}

describe('syncAudioToVideo', () => {
  it('pauses a paused audio element that is currently playing when the video is paused', () => {
    const v = fakeVideo({ paused: true });
    const a = fakeAudio({ paused: false });
    syncAudioToVideo(asEl(v), [asEl(a)]);
    expect(a.pause).toHaveBeenCalled();
    expect(a.play).not.toHaveBeenCalled();
  });

  it('pauses audio when the video has ended', () => {
    const v = fakeVideo({ ended: true, paused: false });
    const a = fakeAudio({ paused: false });
    syncAudioToVideo(asEl(v), [asEl(a)]);
    expect(a.pause).toHaveBeenCalled();
  });

  it('when the video is paused, aligns currentTime only if drift exceeds the threshold', () => {
    const v = fakeVideo({ paused: true, currentTime: 10 });

    const small = fakeAudio({ currentTime: 10.1 });
    syncAudioToVideo(asEl(v), [asEl(small)]);
    expect(small.currentTime).toBe(10.1);

    const big = fakeAudio({ currentTime: 10.5 });
    syncAudioToVideo(asEl(v), [asEl(big)]);
    expect(big.currentTime).toBe(10);
  });

  it('plays a paused audio element while the video is playing', () => {
    const v = fakeVideo({ paused: false });
    const a = fakeAudio({ paused: true });
    syncAudioToVideo(asEl(v), [asEl(a)]);
    expect(a.play).toHaveBeenCalledTimes(1);
  });

  it('does not call play again on an already playing audio element', () => {
    const v = fakeVideo({ paused: false });
    const a = fakeAudio({ paused: false });
    syncAudioToVideo(asEl(v), [asEl(a)]);
    expect(a.play).not.toHaveBeenCalled();
  });

  it('a rejected play() promise does not throw or cause an unhandled rejection', async () => {
    const v = fakeVideo({ paused: false });
    const a = fakeAudio({ paused: true });
    a.play = vi.fn(() => Promise.reject(new Error('NotAllowedError')));
    expect(() => syncAudioToVideo(asEl(v), [asEl(a)])).not.toThrow();
    // Let the rejection settle; an unhandled rejection would fail the run.
    await new Promise((r) => setTimeout(r, 0));
  });

  it('seeks the audio when the video is playing and drift exceeds the threshold', () => {
    const v = fakeVideo({ paused: false, currentTime: 20 });
    const a = fakeAudio({ paused: false, currentTime: 19 });
    syncAudioToVideo(asEl(v), [asEl(a)]);
    expect(a.currentTime).toBe(20);
  });

  it('does not seek when drift is within the default threshold', () => {
    const v = fakeVideo({ paused: false, currentTime: 20 });
    const a = fakeAudio({ paused: false, currentTime: 20.1 });
    syncAudioToVideo(asEl(v), [asEl(a)]);
    expect(a.currentTime).toBe(20.1);
  });

  it('mirrors playbackRate onto every audio element', () => {
    const v = fakeVideo({ playbackRate: 1.5 });
    const a = fakeAudio({ playbackRate: 1 });
    const b = fakeAudio({ playbackRate: 1 });
    syncAudioToVideo(asEl(v), [asEl(a), asEl(b)]);
    expect(a.playbackRate).toBe(1.5);
    expect(b.playbackRate).toBe(1.5);
  });

  it('syncs many audio elements independently', () => {
    const v = fakeVideo({ paused: false, currentTime: 30 });
    const audios = [0, 0.05, 1, 2, 5].map((t) => fakeAudio({ paused: false, currentTime: 30 + t }));
    syncAudioToVideo(asEl(v), audios.map(asEl));
    expect(audios[0].currentTime).toBe(30);
    expect(audios[1].currentTime).toBeCloseTo(30.05, 10);
    expect(audios[2].currentTime).toBe(30);
    expect(audios[3].currentTime).toBe(30);
    expect(audios[4].currentTime).toBe(30);
  });

  it('honours the threshold parameter', () => {
    const v = fakeVideo({ paused: false, currentTime: 10 });
    const loose = fakeAudio({ paused: false, currentTime: 10.5 });
    syncAudioToVideo(asEl(v), [asEl(loose)], 1);
    expect(loose.currentTime).toBe(10.5);

    const tight = fakeAudio({ paused: false, currentTime: 10.05 });
    syncAudioToVideo(asEl(v), [asEl(tight)], 0.01);
    expect(tight.currentTime).toBe(10);
  });
});
