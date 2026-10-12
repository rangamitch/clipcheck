import { describe, expect, it } from 'vitest';
import type { AudioDevice } from '../discovery';
import {
  BITRATE_OPTIONS_KBPS,
  MAX_APPS,
  MAX_APP_NAME_CHARS,
  MAX_TRACKS,
  MIC_TEST_SILENT_DB,
  SELF_EXE,
  addApp,
  appDisplayName,
  bitrateOptions,
  canAddSource,
  canonicalExe,
  enabledTrackKbps,
  findAppIndex,
  isMicUnavailable,
  isSelfExe,
  micDeviceOptions,
  micTestVerdict,
  removeApp,
  selectedMicLabel,
  setAppField,
  sourcesEnabledCount,
  trackPlanPreview,
  type TrackPlanInput,
} from './audio';
import type { AudioAppConfig } from './store';

/** A fully enabled-or-not audio block. Override pieces per test. */
function plan(over: Partial<TrackPlanInput> = {}): TrackPlanInput {
  return {
    separate_tracks: true,
    game: { enabled: true, gain_db: 0 },
    mic: { enabled: true, device: 'default', gain_db: 0, gate_enabled: false, gate_db: -50, ptt_separate_track: false, ptt_mix_gated: true, ptt_release_ms: 250 },
    other_apps: { enabled: false, gain_db: 0 },
    apps: [],
    ...over,
  };
}

function app(exe: string, patch: Partial<AudioAppConfig> = {}): AudioAppConfig {
  return { exe, name: exe.replace(/\.exe$/, ''), enabled: true, gain_db: 0, ...patch };
}

function dev(id: string, name: string, isDefault = false): AudioDevice {
  return { id, name, isDefault };
}

describe('constants', () => {
  it('match the schema limits', () => {
    expect(MAX_TRACKS).toBe(8);
    expect(MAX_APPS).toBe(16);
    expect(MAX_APP_NAME_CHARS).toBe(32);
    expect(SELF_EXE).toBe('clipcheck.exe');
    expect(BITRATE_OPTIONS_KBPS).toEqual([128, 160, 192, 256, 320]);
  });
});

describe('trackPlanPreview (mirrors Audio::track_plan)', () => {
  it('default layout: game and mic on, separate on gives Mix, Game, Microphone', () => {
    const p = trackPlanPreview(plan());
    expect(p.titles).toEqual(['Mix', 'Game', 'Microphone']);
    expect(p.count).toBe(3);
    expect(p.over).toBe(false);
    expect(p.enabledSources).toBe(2);
  });

  it('separate tracks off gives only the Mix track', () => {
    const p = trackPlanPreview(plan({ separate_tracks: false }));
    expect(p.titles).toEqual(['Mix']);
    expect(p.count).toBe(1);
  });

  it('no enabled source gives one silent Mix, even with separate tracks on', () => {
    const p = trackPlanPreview(
      plan({
        game: { enabled: false, gain_db: 0 },
        mic: { enabled: false, device: 'default', gain_db: 0, gate_enabled: false, gate_db: -50, ptt_separate_track: false, ptt_mix_gated: true, ptt_release_ms: 250 },
      }),
    );
    expect(p.titles).toEqual(['Mix']);
    expect(p.count).toBe(1);
    expect(p.enabledSources).toBe(0);
  });

  it('orders sources: game, microphone, apps in list order, other apps', () => {
    const p = trackPlanPreview(
      plan({
        other_apps: { enabled: true, gain_db: 0 },
        apps: [app('zeta.exe', { name: 'Zeta' }), app('off.exe', { enabled: false }), app('alpha.exe', { name: 'Alpha' })],
      }),
    );
    expect(p.titles).toEqual(['Mix', 'Game', 'Microphone', 'Zeta', 'Alpha', 'Other apps']);
  });

  it('a disabled app adds no track', () => {
    const p = trackPlanPreview(plan({ apps: [app('discord.exe', { enabled: false })] }));
    expect(p.titles).toEqual(['Mix', 'Game', 'Microphone']);
  });

  it('exactly 8 tracks is allowed; 7 enabled sources with separate tracks gives 8', () => {
    const apps = [1, 2, 3, 4, 5].map((i) => app(`a${i}.exe`, { name: `App ${i}` }));
    const p = trackPlanPreview(plan({ other_apps: { enabled: false, gain_db: 0 }, apps }));
    expect(p.enabledSources).toBe(7);
    expect(p.count).toBe(8);
    expect(p.over).toBe(false);
  });

  it('8 enabled sources with separate tracks gives 9 tracks and is over the limit', () => {
    const apps = [1, 2, 3, 4, 5, 6].map((i) => app(`a${i}.exe`, { name: `App ${i}` }));
    const p = trackPlanPreview(plan({ apps }));
    expect(p.enabledSources).toBe(8);
    expect(p.count).toBe(9);
    expect(p.over).toBe(true);
  });

  it('8 enabled sources with separate tracks off is fine (one Mix track)', () => {
    const apps = [1, 2, 3, 4, 5, 6].map((i) => app(`a${i}.exe`, { name: `App ${i}` }));
    const p = trackPlanPreview(plan({ separate_tracks: false, apps, other_apps: { enabled: true, gain_db: 0 } }));
    expect(p.count).toBe(1);
    expect(p.over).toBe(false);
  });
});

describe('sourcesEnabledCount and canAddSource', () => {
  it('counts enabled sources only', () => {
    expect(sourcesEnabledCount(plan({ apps: [app('a.exe'), app('b.exe', { enabled: false })] }))).toBe(3);
  });

  it('separate off: a source can always be added', () => {
    const many = plan({ separate_tracks: false, apps: [1, 2, 3, 4, 5, 6, 7, 8].map((i) => app(`a${i}.exe`)) });
    expect(canAddSource(many)).toBe(true);
  });

  it('separate on: allowed while the count is below the limit', () => {
    const seven = plan({ apps: [1, 2, 3, 4, 5].map((i) => app(`a${i}.exe`)) }); // 7 sources -> 8 tracks
    expect(canAddSource(seven)).toBe(false);
    const six = plan({ apps: [1, 2, 3, 4].map((i) => app(`a${i}.exe`)) }); // 6 sources -> 7 tracks
    expect(canAddSource(six)).toBe(true);
  });
});

describe('canonicalExe and isSelfExe', () => {
  it.each([
    ['Discord.EXE', 'discord.exe'],
    ['  game.exe  ', 'game.exe'],
  ])('%j -> %s', (raw, value) => {
    expect(canonicalExe(raw)).toBe(value);
  });

  it.each(['game', '', 'C:\\x\\a.exe', 'a/b.exe', 'game.dll'])('%j is refused', (raw) => {
    expect(canonicalExe(raw)).toBeNull();
  });

  it('recognises ClipCheck in any case', () => {
    expect(isSelfExe('ClipCheck.exe')).toBe(true);
    expect(isSelfExe('discord.exe')).toBe(false);
  });
});

describe('appDisplayName', () => {
  it('uses the trimmed name', () => {
    expect(appDisplayName('  Discord  ', 'discord.exe')).toBe('Discord');
  });

  it('falls back to the exe without .exe when the name is blank', () => {
    expect(appDisplayName('   ', 'spotify.exe')).toBe('spotify');
  });

  it('caps the length at 32 characters', () => {
    expect(appDisplayName('x'.repeat(40), 'a.exe')).toHaveLength(MAX_APP_NAME_CHARS);
  });
});

describe('addApp', () => {
  it('appends an enabled app at 0 dB with the given name', () => {
    const r = addApp([app('game.exe')], { exe: 'Spotify.exe', name: 'Spotify' });
    expect(r).toEqual({
      ok: true,
      apps: [app('game.exe'), { exe: 'spotify.exe', name: 'Spotify', enabled: true, gain_db: 0 }],
    });
  });

  it('keeps the list order and does not mutate the input', () => {
    const before = [app('a.exe')];
    const snapshot = structuredClone(before);
    const r = addApp(before, { exe: 'b.exe', name: 'B' });
    expect(before).toEqual(snapshot);
    expect(r.ok && r.apps.map((a) => a.exe)).toEqual(['a.exe', 'b.exe']);
  });

  it('refuses an exe already listed, ignoring case', () => {
    const r = addApp([app('discord.exe')], { exe: 'DISCORD.exe', name: 'x' });
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain('already in the list');
  });

  it('refuses ClipCheck itself', () => {
    const r = addApp([], { exe: 'ClipCheck.exe', name: 'ClipCheck' });
    expect(r.ok).toBe(false);
  });

  it('refuses an invalid exe name', () => {
    expect(addApp([], { exe: 'discord', name: 'x' }).ok).toBe(false);
    expect(addApp([], { exe: 'C:\\apps\\x.exe', name: 'x' }).ok).toBe(false);
  });

  it('allows exactly 16 apps and refuses the 17th', () => {
    let list: AudioAppConfig[] = [];
    for (let i = 0; i < MAX_APPS; i++) {
      const r = addApp(list, { exe: `app${i}.exe`, name: `App ${i}` });
      expect(r.ok).toBe(true);
      if (r.ok) list = r.apps;
    }
    expect(list).toHaveLength(MAX_APPS);
    const seventeenth = addApp(list, { exe: 'extra.exe', name: 'Extra' });
    expect(seventeenth.ok).toBe(false);
    if (!seventeenth.ok) expect(seventeenth.error).toContain('16');
  });
});

describe('removeApp', () => {
  it('removes the entry by exe, ignoring case', () => {
    expect(removeApp([app('a.exe'), app('b.exe')], 'A.EXE')).toEqual([app('b.exe')]);
  });

  it('an unknown exe leaves the list equal', () => {
    const before = [app('a.exe')];
    expect(removeApp(before, 'zzz.exe')).toEqual(before);
  });

  it('does not mutate the input', () => {
    const before = [app('a.exe'), app('b.exe')];
    removeApp(before, 'a.exe');
    expect(before).toHaveLength(2);
  });
});

describe('setAppField (gain write-through)', () => {
  it('changes gain on the addressed app only', () => {
    const before = [app('a.exe'), app('b.exe', { gain_db: -6 })];
    const out = setAppField(before, 'a.exe', { gain_db: 3 });
    expect(out).toEqual([app('a.exe', { gain_db: 3 }), app('b.exe', { gain_db: -6 })]);
  });

  it('can toggle enabled and rename', () => {
    const out = setAppField([app('a.exe')], 'A.exe', { enabled: false, name: 'Alpha' });
    expect(out[0]).toMatchObject({ enabled: false, name: 'Alpha' });
  });

  it('does not mutate the input', () => {
    const before = [app('a.exe')];
    setAppField(before, 'a.exe', { gain_db: 9 });
    expect(before[0].gain_db).toBe(0);
  });

  it('an unknown exe changes nothing', () => {
    const before = [app('a.exe')];
    expect(setAppField(before, 'z.exe', { gain_db: 9 })).toEqual(before);
  });
});

describe('findAppIndex', () => {
  it('finds the index ignoring case, or -1', () => {
    const list = [app('a.exe'), app('b.exe')];
    expect(findAppIndex(list, 'B.EXE')).toBe(1);
    expect(findAppIndex(list, 'c.exe')).toBe(-1);
  });
});

describe('microphone labels and missing-device option', () => {
  const capture = [dev('{mic-1}', 'USB Mic'), dev('{mic-2}', 'Headset', true)];

  it('default option names the default device', () => {
    expect(selectedMicLabel(capture, 'default')).toBe('Default — Headset');
  });

  it('default option without a default device says just Default', () => {
    expect(selectedMicLabel([dev('{x}', 'Only')], 'default')).toBe('Default');
  });

  it('a saved id shows its device name', () => {
    expect(selectedMicLabel(capture, '{mic-1}')).toBe('USB Mic');
  });

  it('a saved id that is gone shows Unavailable device', () => {
    expect(selectedMicLabel(capture, '{gone}')).toBe('Unavailable device');
  });

  it('options: default first, then each device by id', () => {
    expect(micDeviceOptions(capture, 'default')).toEqual([
      { value: 'default', label: 'Default — Headset' },
      { value: '{mic-1}', label: 'USB Mic' },
      { value: '{mic-2}', label: 'Headset' },
    ]);
  });

  it('keeps a missing saved device as an Unavailable option so the value is not lost', () => {
    const opts = micDeviceOptions(capture, '{gone}');
    expect(opts.map((o) => o.value)).toEqual(['default', '{mic-1}', '{mic-2}', '{gone}']);
    expect(opts[3].label).toBe('Unavailable device');
    expect(opts[3].title).toBeTruthy();
  });

  it('adds no extra option when the saved device is present', () => {
    expect(micDeviceOptions(capture, '{mic-2}')).toHaveLength(3);
  });

  it('isMicUnavailable is false for default, for a present id, and while the list is empty', () => {
    expect(isMicUnavailable(capture, 'default')).toBe(false);
    expect(isMicUnavailable(capture, '{mic-1}')).toBe(false);
    expect(isMicUnavailable([], '{gone}')).toBe(false);
    expect(isMicUnavailable(capture, '{gone}')).toBe(true);
  });
});

describe('enabledTrackKbps (one entry per recorded track)', () => {
  it('default layout: Mix, Game, Microphone at the audio bitrate', () => {
    expect(enabledTrackKbps({ ...plan(), bitrate_kbps: 192 })).toEqual([192, 192, 192]);
  });

  it('separate tracks off: a single Mix entry', () => {
    expect(enabledTrackKbps({ ...plan({ separate_tracks: false }), bitrate_kbps: 256 })).toEqual([256]);
  });

  it('nothing enabled: one silent Mix entry', () => {
    const silent = plan({
      game: { enabled: false, gain_db: 0 },
      mic: { enabled: false, device: 'default', gain_db: 0, gate_enabled: false, gate_db: -50, ptt_separate_track: false, ptt_mix_gated: true, ptt_release_ms: 250 },
    });
    expect(enabledTrackKbps({ ...silent, bitrate_kbps: 128 })).toEqual([128]);
  });
});

describe('bitrateOptions', () => {
  it('returns the standard steps for a standard value', () => {
    expect(bitrateOptions(192)).toEqual([128, 160, 192, 256, 320]);
  });

  it('adds a non-standard saved value in order', () => {
    expect(bitrateOptions(200)).toEqual([128, 160, 192, 200, 256, 320]);
  });
});

describe('micTestVerdict', () => {
  it('treats silence and non-finite peaks as silent', () => {
    expect(micTestVerdict(-100)).toBe('silent');
    expect(micTestVerdict(-60)).toBe('silent');
    expect(micTestVerdict(NaN)).toBe('silent');
  });

  it('treats audible peaks as ok', () => {
    expect(micTestVerdict(-59.9)).toBe('ok');
    expect(micTestVerdict(-6)).toBe('ok');
  });
});

describe('micTestVerdict boundaries', () => {
  it('MIC_TEST_SILENT_DB is -60', () => {
    expect(MIC_TEST_SILENT_DB).toBe(-60);
  });

  it.each([
    [-60, 'silent'],
    [-59.99, 'ok'],
    [-100, 'silent'],
    [0, 'ok'],
    [Infinity, 'silent'],
    [-Infinity, 'silent'],
    [NaN, 'silent'],
  ] as const)('micTestVerdict(%s) is %s', (peak, verdict) => {
    expect(micTestVerdict(peak)).toBe(verdict);
  });
});
