import { describe, expect, it } from 'vitest';
import { updateClip, addClip, removeClip } from './clips';
import { addApp, removeApp, setAppField } from './audio';
import { cloneJson, diffPatch, setPath } from './pure';
import type { ConfigDoc } from './store';

// Generic diffPatch, getPath and setPath behaviour lives in pure.test.ts.
// This file only covers the shape each settings tab sends to the daemon.

/** A config as the daemon returns it, with the sections the tabs edit. */
function savedDoc(): ConfigDoc {
  return {
    config_version: 1,
    general: { autostart: false, close_behavior: 'minimize_to_tray', start_minimized: true },
    storage: { root: 'C:\\Videos', quota_gb: 0, filename_template: '{game} {kind}' },
    detection: { mode: 'auto', allow: [], block: [], stop_grace_s: 10 },
    video: {
      encoder: 'auto',
      codec: 'h264',
      container: 'mp4',
      preset: 'medium',
      resolution: '1080p',
      fps: 60,
      rate_control: 'quality',
      quality_cq: 23,
      bitrate_kbps: 12000,
      max_bitrate_kbps: 15000,
      keyframe_interval_s: 2,
      cursor: true,
    } as unknown as ConfigDoc['video'],
    audio: {
      codec: 'aac',
      bitrate_kbps: 192,
      separate_tracks: true,
      output_device: 'default',
      game: { enabled: true, gain_db: 0 },
      mic: { enabled: true, device: 'default', gain_db: 0, gate_enabled: false, gate_db: -50, ptt_separate_track: false, ptt_mix_gated: true, ptt_release_ms: 250 },
      other_apps: { enabled: false, gain_db: 0 },
      apps: [{ exe: 'discord.exe', name: 'Discord', enabled: true, gain_db: 0 }],
    } as ConfigDoc['audio'],
    replay: { enabled: true, max_seconds: 300, storage: 'ram', location: '%TEMP%', ram_max_mb: 1024 },
    hotkeys: {
      clips: [
        { name: 'Clip 1', keys: 'Alt+F9', seconds: 30 },
        { name: 'Clip 2', keys: 'Alt+F10', seconds: 60 },
      ],
      session: {
        split: 'Alt+F8',
        stop_start: 'Alt+F7',
        toggle_buffer: 'Alt+F6',
        mute_mic: '',
        mute_mic_mode: 'toggle',
        push_to_talk: [],
      },
    },
  } as ConfigDoc;
}

/** The draft starts as a deep copy of the saved config, as the store does. */
function draftOf(saved: ConfigDoc): ConfigDoc {
  return cloneJson(saved);
}

describe('Audio tab: audio.apps array edits send the whole array', () => {
  it('editing one app gain sends the full audio.apps array, with only that gain changed', () => {
    const saved = savedDoc();
    saved.audio!.apps.push({ exe: 'spotify.exe', name: 'Spotify', enabled: true, gain_db: 0 });
    const draft = draftOf(saved);
    const next = setAppField(draft.audio!.apps, 'spotify.exe', { gain_db: -6 });
    const patch = diffPatch(saved, setPath(draft, 'audio.apps', next));
    expect(patch).toEqual({
      audio: {
        apps: [
          { exe: 'discord.exe', name: 'Discord', enabled: true, gain_db: 0 },
          { exe: 'spotify.exe', name: 'Spotify', enabled: true, gain_db: -6 },
        ],
      },
    });
  });

  it('adding an app sends the longer array', () => {
    const saved = savedDoc();
    const draft = draftOf(saved);
    const added = addApp(draft.audio!.apps, { exe: 'Spotify.exe', name: 'Spotify' });
    expect(added.ok).toBe(true);
    if (!added.ok) return;
    const patch = diffPatch(saved, setPath(draft, 'audio.apps', added.apps));
    expect((patch.audio as { apps: unknown[] }).apps).toHaveLength(2);
  });

  it('removing an app sends the shorter array', () => {
    const saved = savedDoc();
    saved.audio!.apps.push({ exe: 'spotify.exe', name: 'Spotify', enabled: true, gain_db: 0 });
    const draft = draftOf(saved);
    const patch = diffPatch(saved, setPath(draft, 'audio.apps', removeApp(draft.audio!.apps, 'discord.exe')));
    expect((patch.audio as { apps: Array<{ exe: string }> }).apps).toEqual([
      { exe: 'spotify.exe', name: 'Spotify', enabled: true, gain_db: 0 },
    ]);
  });

  it('setting an app gain to its saved value gives an empty patch', () => {
    const saved = savedDoc();
    const draft = draftOf(saved);
    const patch = diffPatch(saved, setPath(draft, 'audio.apps', setAppField(draft.audio!.apps, 'discord.exe', { gain_db: 0 })));
    expect(patch).toEqual({});
  });
});

describe('Audio tab: mic, game and track leaves send only the changed leaf', () => {
  it('toggling mic.enabled sends only that leaf', () => {
    const saved = savedDoc();
    const draft = setPath(draftOf(saved), 'audio.mic.enabled', false);
    expect(diffPatch(saved, draft)).toEqual({ audio: { mic: { enabled: false } } });
  });

  it('the noise gate threshold sends only gate_db', () => {
    const saved = savedDoc();
    const draft = setPath(draftOf(saved), 'audio.mic.gate_db', -62);
    expect(diffPatch(saved, draft)).toEqual({ audio: { mic: { gate_db: -62 } } });
  });

  it('a game gain change sends only audio.game.gain_db', () => {
    const saved = savedDoc();
    const draft = setPath(draftOf(saved), 'audio.game.gain_db', -6);
    expect(diffPatch(saved, draft)).toEqual({ audio: { game: { gain_db: -6 } } });
  });

  it('turning off push to talk applies to the Mix track sends only that leaf', () => {
    const saved = savedDoc();
    const draft = setPath(draftOf(saved), 'audio.mic.ptt_mix_gated', false);
    expect(diffPatch(saved, draft)).toEqual({ audio: { mic: { ptt_mix_gated: false } } });
  });

  it('turning separate tracks off sends only that flag', () => {
    const saved = savedDoc();
    const draft = setPath(draftOf(saved), 'audio.separate_tracks', false);
    expect(diffPatch(saved, draft)).toEqual({ audio: { separate_tracks: false } });
  });

  it('a mic device id sends only mic.device', () => {
    const saved = savedDoc();
    const draft = setPath(draftOf(saved), 'audio.mic.device', '{abc}');
    expect(diffPatch(saved, draft)).toEqual({ audio: { mic: { device: '{abc}' } } });
  });
});

describe('Hotkeys tab: hotkeys.session keys', () => {
  it('unbinding split sends an empty string under hotkeys.session', () => {
    const saved = savedDoc();
    const draft = setPath(draftOf(saved), 'hotkeys.session.split', '');
    expect(diffPatch(saved, draft)).toEqual({ hotkeys: { session: { split: '' } } });
  });

  it('binding an empty session key sends just that key', () => {
    const saved = savedDoc();
    const draft = setPath(draftOf(saved), 'hotkeys.session.mute_mic', 'Alt+F5');
    expect(diffPatch(saved, draft)).toEqual({ hotkeys: { session: { mute_mic: 'Alt+F5' } } });
  });

  it('changing mute mode sends just that field', () => {
    const saved = savedDoc();
    const draft = setPath(draftOf(saved), 'hotkeys.session.mute_mic_mode', 'push_to_talk');
    expect(diffPatch(saved, draft)).toEqual({ hotkeys: { session: { mute_mic_mode: 'push_to_talk' } } });
  });

  it('adding a push-to-talk key sends the whole push_to_talk array', () => {
    const saved = savedDoc();
    const draft = setPath(draftOf(saved), 'hotkeys.session.push_to_talk', ['Mouse4']);
    expect(diffPatch(saved, draft)).toEqual({ hotkeys: { session: { push_to_talk: ['Mouse4'] } } });
  });

  it('a second push-to-talk key sends both keys in order', () => {
    const saved = savedDoc();
    saved.hotkeys!.session.push_to_talk = ['Mouse4'];
    const draft = setPath(draftOf(saved), 'hotkeys.session.push_to_talk', ['Mouse4', 'Ctrl+Space']);
    expect(diffPatch(saved, draft)).toEqual({
      hotkeys: { session: { push_to_talk: ['Mouse4', 'Ctrl+Space'] } },
    });
  });

  it('removing the only push-to-talk key sends an empty array, not null', () => {
    const saved = savedDoc();
    saved.hotkeys!.session.push_to_talk = ['Mouse5'];
    const draft = setPath(draftOf(saved), 'hotkeys.session.push_to_talk', []);
    expect(diffPatch(saved, draft)).toEqual({ hotkeys: { session: { push_to_talk: [] } } });
  });
});

describe('Hotkeys tab: hotkeys.clips array edits send the whole array', () => {
  it('editing clip 1 seconds sends both clips', () => {
    const saved = savedDoc();
    const draft = draftOf(saved);
    const next = updateClip(draft.hotkeys!.clips, 1, { seconds: 120 });
    expect(diffPatch(saved, setPath(draft, 'hotkeys.clips', next))).toEqual({
      hotkeys: {
        clips: [
          { name: 'Clip 1', keys: 'Alt+F9', seconds: 30 },
          { name: 'Clip 2', keys: 'Alt+F10', seconds: 120 },
        ],
      },
    });
  });

  it('adding a clip sends three entries with the new one last', () => {
    const saved = savedDoc();
    const draft = draftOf(saved);
    const patch = diffPatch(saved, setPath(draft, 'hotkeys.clips', addClip(draft.hotkeys!.clips)));
    const clips = (patch.hotkeys as { clips: Array<{ name: string; keys: string; press: string }> }).clips;
    expect(clips).toHaveLength(3);
    expect(clips[2]).toEqual({ name: 'Clip 3', keys: 'Alt+F6', seconds: 30, press: 'tap' });
  });

  it('removing a clip sends the shorter array', () => {
    const saved = savedDoc();
    const draft = draftOf(saved);
    const patch = diffPatch(saved, setPath(draft, 'hotkeys.clips', removeClip(draft.hotkeys!.clips, 0)));
    expect((patch.hotkeys as { clips: unknown[] }).clips).toEqual([{ name: 'Clip 2', keys: 'Alt+F10', seconds: 60 }]);
  });
});

describe('Video and Replay tabs: one leaf per tab section', () => {
  it('changing the video preset sends only video.preset', () => {
    const saved = savedDoc();
    const draft = setPath(draftOf(saved), 'video.preset', 'custom');
    expect(diffPatch(saved, draft)).toEqual({ video: { preset: 'custom' } });
  });

  it('changing the replay length sends only replay.max_seconds', () => {
    const saved = savedDoc();
    const draft = setPath(draftOf(saved), 'replay.max_seconds', 600);
    expect(diffPatch(saved, draft)).toEqual({ replay: { max_seconds: 600 } });
  });
});
