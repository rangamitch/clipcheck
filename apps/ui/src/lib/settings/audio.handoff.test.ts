import { describe, expect, it } from 'vitest';
import type { AudioDevice } from '../discovery';
import type { AudioAppConfig } from './store';
import {
  MAX_APPS,
  MAX_TRACKS,
  GAIN_MAX_DB,
  GAIN_MIN_DB,
  GATE_MAX_DB,
  GATE_MIN_DB,
  addApp,
  appDisplayName,
  bitrateOptions,
  canAddSource,
  canonicalExe,
  enabledTrackKbps,
  isMicUnavailable,
  isSelfExe,
  micDeviceOptions,
  removeApp,
  setAppField,
  sourcesEnabledCount,
  trackPlanPreview,
  type TrackPlanInput,
} from './audio';

type Plan = {
  separate?: boolean;
  game?: boolean;
  mic?: boolean;
  other?: boolean;
  apps?: { name: string; enabled: boolean }[];
};

/** Builds a track-plan input. Defaults: separate on, every source off, no apps. */
function plan(p: Plan = {}): TrackPlanInput {
  return {
    separate_tracks: p.separate ?? true,
    game: { enabled: p.game ?? false },
    mic: { enabled: p.mic ?? false },
    other_apps: { enabled: p.other ?? false },
    apps: p.apps ?? [],
  } as unknown as TrackPlanInput;
}

/** Makes `n` enabled apps named A1..An. */
function nApps(n: number): { name: string; enabled: boolean }[] {
  return Array.from({ length: n }, (_, i) => ({ name: `A${i + 1}`, enabled: true }));
}

function app(exe: string, name = 'X', enabled = true, gain_db = 0): AudioAppConfig {
  return { exe, name, enabled, gain_db } as AudioAppConfig;
}

function dev(id: string, name: string, isDefault = false): AudioDevice {
  return { id, name, isDefault };
}

describe('trackPlanPreview', () => {
  it('default layout: Mix plus one track per enabled source, in order', () => {
    const p = trackPlanPreview(plan({ game: true, mic: true, other: true }));
    expect(p.titles).toEqual(['Mix', 'Game', 'Microphone', 'Other apps']);
    expect(p.count).toBe(4);
    expect(p.over).toBe(false);
    expect(p.enabledSources).toBe(3);
  });

  it('separate tracks off yields one Mix track', () => {
    const p = trackPlanPreview(plan({ separate: false, game: true, mic: true, other: true }));
    expect(p.titles).toEqual(['Mix']);
    expect(p.count).toBe(1);
    expect(p.enabledSources).toBe(3);
  });

  it('no enabled source yields a single Mix track even with separate on', () => {
    const p = trackPlanPreview(plan({ separate: true, apps: [{ name: 'Off', enabled: false }] }));
    expect(p.titles).toEqual(['Mix']);
    expect(p.enabledSources).toBe(0);
    expect(p.over).toBe(false);
  });

  it('7 enabled sources with separate on give 8 tracks, not over', () => {
    const p = trackPlanPreview(plan({ game: true, mic: true, other: true, apps: nApps(4) }));
    expect(p.enabledSources).toBe(7);
    expect(p.count).toBe(8);
    expect(p.over).toBe(false);
  });

  it('8 enabled sources give 9 tracks and are over', () => {
    const p = trackPlanPreview(plan({ game: true, mic: true, other: true, apps: nApps(5) }));
    expect(p.enabledSources).toBe(8);
    expect(p.count).toBe(9);
    expect(p.count).toBe(MAX_TRACKS + 1);
    expect(p.over).toBe(true);
  });

  it('apps with enabled:false are excluded', () => {
    const p = trackPlanPreview(
      plan({
        apps: [
          { name: 'Discord', enabled: false },
          { name: 'Spotify', enabled: true },
        ],
      }),
    );
    expect(p.titles).toEqual(['Mix', 'Spotify']);
  });

  it('keeps app order between game/mic and other apps', () => {
    const p = trackPlanPreview(
      plan({
        game: true,
        mic: true,
        other: true,
        apps: [
          { name: 'Second', enabled: true },
          { name: 'First', enabled: true },
        ],
      }),
    );
    expect(p.titles).toEqual(['Mix', 'Game', 'Microphone', 'Second', 'First', 'Other apps']);
  });

  it('a duplicate app name still gets its own track', () => {
    const p = trackPlanPreview(
      plan({
        apps: [
          { name: 'Discord', enabled: true },
          { name: 'Discord', enabled: true },
        ],
      }),
    );
    expect(p.titles).toEqual(['Mix', 'Discord', 'Discord']);
    expect(p.count).toBe(3);
  });
});

describe('sourcesEnabledCount and canAddSource', () => {
  it('sourcesEnabledCount matches trackPlanPreview.enabledSources', () => {
    expect(sourcesEnabledCount(plan({ game: true, apps: nApps(2) }))).toBe(3);
  });

  it.each([
    [6, true], // 6 sources -> 7 tracks, one more fits
    [7, false], // 7 sources -> 8 tracks, full
  ])('separate on with %i sources: canAddSource is %s', (sources, expected) => {
    expect(canAddSource(plan({ apps: nApps(sources) }))).toBe(expected);
  });

  it('separate off always allows another source, even with many enabled', () => {
    expect(canAddSource(plan({ separate: false, apps: nApps(16) }))).toBe(true);
  });
});

describe('addApp', () => {
  it('appends an enabled entry at 0 dB with canonical exe', () => {
    const r = addApp([], { exe: '  Game.EXE ' });
    expect(r).toEqual({ ok: true, apps: [{ exe: 'game.exe', name: 'game', enabled: true, gain_db: 0 }] });
  });

  it('de-duplicates case-insensitively', () => {
    const r = addApp([app('game.exe')], { exe: 'GAME.exe' });
    expect(r.ok).toBe(false);
  });

  it('refuses the 17th app and accepts the 16th', () => {
    const full = Array.from({ length: MAX_APPS - 1 }, (_, i) => app(`g${i}.exe`));
    expect(addApp(full, { exe: 'last.exe' }).ok).toBe(true);
    const sixteen = [...full, app('last.exe')];
    expect(addApp(sixteen, { exe: 'extra.exe' }).ok).toBe(false);
  });

  it.each(['ClipCheck.exe', 'CLIPCHECK.EXE', '  clipcheck.exe  '])('refuses ClipCheck itself: %s', (exe) => {
    expect(addApp([], { exe }).ok).toBe(false);
  });

  it.each(['game', 'a/b.exe', 'C:\\x.exe'])('refuses invalid exe %s', (exe) => {
    expect(addApp([], { exe }).ok).toBe(false);
  });

  it('trims the name and caps it at 32 characters', () => {
    const r = addApp([], { exe: 'g.exe', name: `  ${'n'.repeat(40)}  ` });
    expect(r.ok).toBe(true);
    if (r.ok) expect(r.apps[0].name).toBe('n'.repeat(32));
  });

  it('a blank name falls back to the exe stem', () => {
    const r = addApp([], { exe: 'Game.exe', name: '   ' });
    expect(r.ok && r.apps[0].name).toBe('game');
  });

  it('does not mutate the input array or its entries', () => {
    const input = [app('a.exe', 'A', true, 3)];
    const snapshot = JSON.parse(JSON.stringify(input));
    addApp(input, { exe: 'b.exe' });
    expect(input).toEqual(snapshot);
    expect(input).toHaveLength(1);
  });

  it('result entries are fresh objects, not shared with the input', () => {
    const input = [app('a.exe', 'A', true, 3)];
    const r = addApp(input, { exe: 'b.exe' });
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    r.apps[0].gain_db = 9;
    expect(input[0].gain_db).toBe(3);
  });
});

describe('removeApp', () => {
  it('removes the entry matched case-insensitively', () => {
    const out = removeApp([app('a.exe'), app('b.exe')], 'A.EXE');
    expect(out.map((a) => a.exe)).toEqual(['b.exe']);
  });

  it('unknown exe leaves the list unchanged', () => {
    const out = removeApp([app('a.exe')], 'zzz.exe');
    expect(out).toEqual([app('a.exe')]);
  });

  it('does not mutate the input', () => {
    const input = [app('a.exe'), app('b.exe')];
    removeApp(input, 'a.exe');
    expect(input.map((a) => a.exe)).toEqual(['a.exe', 'b.exe']);
  });
});

describe('setAppField', () => {
  it('changes only the addressed entry', () => {
    const input = [app('a.exe', 'A', true, 0), app('b.exe', 'B', true, 0)];
    const out = setAppField(input, 'B.EXE', { gain_db: -6, enabled: false, name: 'Bee' });
    expect(out[0]).toEqual(app('a.exe', 'A', true, 0));
    expect(out[1]).toEqual(app('b.exe', 'Bee', false, -6));
    expect(input[1].gain_db).toBe(0);
  });

  it('unknown exe is a no-op', () => {
    const input = [app('a.exe', 'A', true, 2)];
    expect(setAppField(input, 'zzz.exe', { gain_db: 5 })).toEqual(input);
  });

  it('passes out-of-range gain through unchanged (no clamping here)', () => {
    const out = setAppField([app('a.exe')], 'a.exe', { gain_db: 150 });
    expect(out[0].gain_db).toBe(150);
  });
});

describe('micDeviceOptions and isMicUnavailable', () => {
  const capture = [dev('c1', 'Mic One', true), dev('c2', 'Mic Two')];

  it('saved id present: no extra option', () => {
    const opts = micDeviceOptions(capture, 'c2');
    expect(opts.map((o) => o.value)).toEqual(['default', 'c1', 'c2']);
    expect(opts.some((o) => o.label === 'Unavailable device')).toBe(false);
    expect(isMicUnavailable(capture, 'c2')).toBe(false);
  });

  it('saved id missing: adds an Unavailable option that keeps the value', () => {
    const opts = micDeviceOptions(capture, 'gone');
    const last = opts[opts.length - 1];
    expect(last.value).toBe('gone');
    expect(last.label).toBe('Unavailable device');
    expect(last.title).toBeTruthy();
    expect(opts.filter((o) => o.label === 'Unavailable device')).toHaveLength(1);
    expect(isMicUnavailable(capture, 'gone')).toBe(true);
  });

  it('saved "default" never adds an unavailable option', () => {
    const opts = micDeviceOptions(capture, 'default');
    expect(opts.map((o) => o.value)).toEqual(['default', 'c1', 'c2']);
    expect(isMicUnavailable(capture, 'default')).toBe(false);
  });

  it('empty capture list with a saved id still gives one Unavailable option (documented behavior)', () => {
    const opts = micDeviceOptions([], 'c9');
    expect(opts.map((o) => o.value)).toEqual(['default', 'c9']);
    expect(opts[1].label).toBe('Unavailable device');
  });

  it('isMicUnavailable is false when the capture list is empty', () => {
    expect(isMicUnavailable([], 'c9')).toBe(false);
  });
});

describe('enabledTrackKbps', () => {
  it('returns one bitrate per recorded track', () => {
    const audio = { ...plan({ game: true, mic: true, other: true }), bitrate_kbps: 192 } as TrackPlanInput & {
      bitrate_kbps: number;
    };
    expect(enabledTrackKbps(audio)).toEqual([192, 192, 192, 192]);
  });

  it('a non-separate layout gives one entry', () => {
    const audio = { ...plan({ separate: false, game: true }), bitrate_kbps: 256 } as TrackPlanInput & {
      bitrate_kbps: number;
    };
    expect(enabledTrackKbps(audio)).toEqual([256]);
  });
});

describe('bitrateOptions', () => {
  it('standard value returns the standard list unchanged', () => {
    expect(bitrateOptions(192)).toEqual([128, 160, 192, 256, 320]);
  });

  it.each([
    [224, [128, 160, 192, 224, 256, 320]],
    [96, [96, 128, 160, 192, 256, 320]],
  ])('non-standard %i is added and sorted', (current, expected) => {
    expect(bitrateOptions(current)).toEqual(expected);
  });

  it('returns a copy, so callers cannot change the constant list', () => {
    bitrateOptions(192).push(999);
    expect(bitrateOptions(192)).not.toContain(999);
  });
});

describe('canonicalExe and isSelfExe', () => {
  it('canonical form is lowercase trimmed .exe, null for invalid', () => {
    expect(canonicalExe(' Foo.EXE ')).toBe('foo.exe');
    expect(canonicalExe('foo')).toBeNull();
  });

  it('isSelfExe is case-insensitive', () => {
    expect(isSelfExe('ClipCheck.exe')).toBe(true);
    expect(isSelfExe('other.exe')).toBe(false);
  });
});

describe('appDisplayName', () => {
  it('trims and falls back to the exe stem', () => {
    expect(appDisplayName('  Spotify ', 'spotify.exe')).toBe('Spotify');
    expect(appDisplayName('', 'Spotify.EXE')).toBe('Spotify');
  });
});

describe('constants used by the tab', () => {
  it('gain and gate ranges match the schema', () => {
    expect([GAIN_MIN_DB, GAIN_MAX_DB]).toEqual([-60, 12]);
    expect([GATE_MIN_DB, GATE_MAX_DB]).toEqual([-100, -20]);
  });
});
