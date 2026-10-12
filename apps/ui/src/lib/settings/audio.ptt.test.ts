import { describe, expect, it } from 'vitest';
import {
  micTrackSummary,
  pttModeOf,
  pttSeparateOf,
  trackPlanPreview,
  type TrackPlanInput,
} from './audio';
import type { AudioAppConfig, AudioConfig } from './store';

/** A layout with game and mic on, separate tracks on. Override pieces per test. */
function plan(over: Partial<TrackPlanInput> = {}, ptt = false): TrackPlanInput {
  return {
    separate_tracks: true,
    game: { enabled: true, gain_db: 0 },
    mic: {
      enabled: true,
      device: 'default',
      gain_db: 0,
      gate_enabled: false,
      gate_db: -50,
      ptt_separate_track: ptt,
      ptt_mix_gated: true,
      ptt_release_ms: 250,
    },
    other_apps: { enabled: false, gain_db: 0 },
    apps: [],
    ...over,
  };
}

function app(exe: string, patch: Partial<AudioAppConfig> = {}): AudioAppConfig {
  return { exe, name: exe.replace(/\.exe$/, ''), enabled: true, gain_db: 0, ...patch };
}

describe('trackPlanPreview with push-to-talk on its own track', () => {
  it('off: no Push to talk track, layout unchanged', () => {
    const p = trackPlanPreview(plan({}, false));
    expect(p.titles).toEqual(['Mix', 'Game', 'Microphone']);
    expect(p.titles).not.toContain('Push to talk');
    expect(p.count).toBe(3);
    expect(p.enabledSources).toBe(2);
  });

  it('on: adds Push to talk right after Microphone', () => {
    const p = trackPlanPreview(plan({}, true));
    expect(p.titles).toEqual(['Mix', 'Game', 'Microphone', 'Push to talk']);
    expect(p.count).toBe(4);
    expect(p.enabledSources).toBe(3);
    expect(p.over).toBe(false);
  });

  it('on: Push to talk comes before apps and other apps', () => {
    const p = trackPlanPreview(
      plan(
        {
          other_apps: { enabled: true, gain_db: 0 },
          apps: [app('zeta.exe', { name: 'Zeta' }), app('off.exe', { enabled: false })],
        },
        true,
      ),
    );
    expect(p.titles).toEqual(['Mix', 'Game', 'Microphone', 'Push to talk', 'Zeta', 'Other apps']);
    expect(p.count).toBe(6);
  });

  it('on with separate tracks off: still only the Mix track', () => {
    const p = trackPlanPreview(plan({ separate_tracks: false }, true));
    expect(p.titles).toEqual(['Mix']);
    expect(p.count).toBe(1);
  });

  it('on with mic disabled: no Push to talk track', () => {
    const p = trackPlanPreview(
      plan(
        {
          mic: { enabled: false, device: 'default', gain_db: 0, gate_enabled: false, gate_db: -50, ptt_separate_track: true, ptt_mix_gated: true, ptt_release_ms: 250 },
        },
        true,
      ),
    );
    expect(p.titles).toEqual(['Mix', 'Game']);
    expect(p.count).toBe(2);
    expect(p.enabledSources).toBe(1);
  });

  it('on with only the mic enabled: Mix, Microphone, Push to talk', () => {
    const p = trackPlanPreview(plan({ game: { enabled: false, gain_db: 0 } }, true));
    expect(p.titles).toEqual(['Mix', 'Microphone', 'Push to talk']);
    expect(p.enabledSources).toBe(2);
  });

  it('on at the 8-track limit: the Push to talk track counts toward it', () => {
    const apps = [1, 2, 3, 4].map((i) => app(`a${i}.exe`, { name: `App ${i}` }));
    // game, mic, push to talk, 4 apps = 7 sources -> 8 tracks, still allowed
    const ok = trackPlanPreview(plan({ apps }, true));
    expect(ok.enabledSources).toBe(7);
    expect(ok.count).toBe(8);
    expect(ok.over).toBe(false);

    // game, mic, push to talk, 5 apps = 8 sources -> 9 tracks, over the limit
    const more = trackPlanPreview(plan({ apps: [...apps, app('a5.exe', { name: 'App 5' })] }, true));
    expect(more.enabledSources).toBe(8);
    expect(more.count).toBe(9);
    expect(more.over).toBe(true);
  });
});
describe('push-to-talk mode and resulting tracks', () => {
  /** Audio config with the microphone on, for the mode and summary helpers. */
  function audioCfg(
    opts: { separate?: boolean; ptt?: boolean; gated?: boolean; micOn?: boolean } = {},
  ): Pick<AudioConfig, 'separate_tracks' | 'mic'> {
    return {
      separate_tracks: opts.separate ?? true,
      mic: {
        enabled: opts.micOn ?? true,
        device: 'default',
        gain_db: 0,
        gate_enabled: false,
        gate_db: -50,
        ptt_separate_track: opts.ptt ?? false,
        ptt_mix_gated: opts.gated ?? true,
        ptt_release_ms: 250,
      },
    };
  }

  it('pttModeOf maps ptt_separate_track false to ptt_only and true to ptt_and_open', () => {
    expect(pttModeOf(audioCfg({ ptt: false }))).toBe('ptt_only');
    expect(pttModeOf(audioCfg({ ptt: true }))).toBe('ptt_and_open');
  });

  it('pttSeparateOf is the inverse of pttModeOf', () => {
    expect(pttSeparateOf('ptt_only')).toBe(false);
    expect(pttSeparateOf('ptt_and_open')).toBe(true);
  });

  it('ptt_only with separate tracks on lists Mix and Microphone, both push to talk', () => {
    expect(micTrackSummary(audioCfg({ separate: true, ptt: false }))).toBe(
      'Mix (push to talk), Microphone (push to talk)',
    );
  });

  it('ptt_only with separate tracks off lists only the Mix track', () => {
    expect(micTrackSummary(audioCfg({ separate: false, ptt: false }))).toBe('Mix (push to talk)');
  });

  it('ptt_and_open with mix gated lists the Mix as push to talk and the open mic tracks', () => {
    expect(micTrackSummary(audioCfg({ separate: true, ptt: true, gated: true }))).toBe(
      'Mix (push to talk), Microphone (open mic), Push to talk (push to talk)',
    );
  });

  it('ptt_and_open with mix not gated puts the open mic on the Mix track', () => {
    expect(micTrackSummary(audioCfg({ separate: true, ptt: true, gated: false }))).toBe(
      'Mix (open mic), Microphone (open mic), Push to talk (push to talk)',
    );
  });

  it('ptt_and_open without separate tracks adds no extra tracks', () => {
    expect(micTrackSummary(audioCfg({ separate: false, ptt: true, gated: false }))).toBe('Mix (open mic)');
  });

  it('says so when the microphone is off', () => {
    expect(micTrackSummary(audioCfg({ micOn: false }))).toBe(
      'Microphone is off, so no microphone is recorded.',
    );
  });
});
