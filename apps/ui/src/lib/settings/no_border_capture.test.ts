import { describe, expect, it } from 'vitest';
import { cloneJson, diffPatch, getPath, setPath } from './pure';
import { VIDEO_DEFAULTS, videoDefaultWrites } from './video';
import type { ConfigDoc } from './store';

// Black-box checks for the `video.no_border_capture` boolean.
// The Svelte toggle itself is not rendered here (no component render harness in
// this project); these tests cover the defaults, the draft path, and the patch
// shape the store sends, mirroring how `video.cursor` is handled.

/** A minimal saved config with the video section the Video tab edits. */
function savedDoc(video: Record<string, unknown> = {}): ConfigDoc {
  return {
    config_version: 1,
    video: {
      encoder: 'auto',
      codec: 'h264',
      preset: 'high',
      resolution: '1080p',
      fps: 60,
      cursor: true,
      ...video,
    },
  } as unknown as ConfigDoc;
}

/** The Toggle is bound to `video.no_border_capture ?? false`; mirror that expression. */
function toggleChecked(video: { no_border_capture?: boolean } | undefined): boolean {
  return (video?.no_border_capture ?? false) === true;
}

describe('video.no_border_capture defaults', () => {
  it('VIDEO_DEFAULTS sets no_border_capture to false', () => {
    expect(VIDEO_DEFAULTS.no_border_capture).toBe(false);
  });

  it('videoDefaultWrites includes a video.no_border_capture=false write', () => {
    const writes = videoDefaultWrites();
    expect(writes).toContainEqual(['video.no_border_capture', false]);
  });

  it('restoring defaults on a saved config with the flag on sends it back to false', () => {
    // Start from the full defaults so only the flag differs from the defaults.
    const saved = savedDoc({ ...VIDEO_DEFAULTS, no_border_capture: true });
    let draft = cloneJson(saved);
    for (const [path, value] of videoDefaultWrites()) {
      draft = setPath(draft, path, value);
    }
    expect(getPath(draft, 'video.no_border_capture')).toBe(false);
    expect(diffPatch(saved, draft)).toEqual({ video: { no_border_capture: false } });
  });
});

describe('video.no_border_capture toggle state', () => {
  it('is unchecked when the saved value is false', () => {
    expect(toggleChecked(savedDoc({ no_border_capture: false }).video)).toBe(false);
  });

  it('is unchecked when the field is missing (older configs)', () => {
    expect(toggleChecked(savedDoc().video)).toBe(false);
  });

  it('is checked when the saved value is true', () => {
    expect(toggleChecked(savedDoc({ no_border_capture: true }).video)).toBe(true);
  });
});

describe('video.no_border_capture write path', () => {
  it('turning the toggle on sends only { video: { no_border_capture: true } }', () => {
    const saved = savedDoc();
    const draft = setPath(cloneJson(saved), 'video.no_border_capture', true);
    expect(diffPatch(saved, draft)).toEqual({ video: { no_border_capture: true } });
  });

  it('turning the toggle on does not touch the sibling cursor flag', () => {
    const saved = savedDoc({ cursor: true });
    const draft = setPath(cloneJson(saved), 'video.no_border_capture', true);
    const patch = diffPatch(saved, draft);
    expect(patch).toEqual({ video: { no_border_capture: true } });
    expect(getPath(draft, 'video.cursor')).toBe(true);
  });

  it('turning the toggle on when already saved as true gives an empty patch', () => {
    const saved = savedDoc({ no_border_capture: true });
    const draft = setPath(cloneJson(saved), 'video.no_border_capture', true);
    expect(diffPatch(saved, draft)).toEqual({});
  });

  it('setting it true does not mutate the saved document', () => {
    const saved = savedDoc();
    setPath(cloneJson(saved), 'video.no_border_capture', true);
    expect(getPath(saved, 'video.no_border_capture')).toBeUndefined();
  });

  it('cursor is patched like no_border_capture: a single leaf, same parent', () => {
    const saved = savedDoc();
    const draft = setPath(cloneJson(saved), 'video.cursor', false);
    expect(diffPatch(saved, draft)).toEqual({ video: { cursor: false } });
  });
});

describe('Capture Method control wiring', () => {
  it('binds the Segmented control to video.no_border_capture with quoted literals', async () => {
    const { readFileSync } = await import('node:fs');
    const src = readFileSync('src/lib/settings/tabs/VideoTab.svelte', 'utf8');
    expect(src).toContain("value={video.no_border_capture ? 'experimental' : 'standard'}");
    expect(src).toContain("setPath('video.no_border_capture', v === 'experimental')");
    expect(src).toContain("label: 'Standard Capture'");
    expect(src).toContain("label: 'Experimental Capture'");
  });
});
