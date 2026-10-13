import { describe, expect, it } from 'vitest';
import { directLayout, type DirectLayoutOptions } from './compose';
import { directAllowed, shouldEnterDirect, shouldLeaveDirect, type DirectState } from './direct';
import type { VideoLayer } from '../types';
import { defaultTransform } from '../types';

function layer(over: Partial<VideoLayer> = {}): VideoLayer {
  return {
    clip_id: 'c1',
    kind: 'video',
    media_id: 'm1',
    z: 0,
    src_time: 0,
    speed: 1,
    reverse: false,
    transform: defaultTransform(),
    opacity: 1,
    transition: null,
    clip_start: 0,
    text: null,
    solid: null,
    ...over,
  };
}

const opts = (o: Partial<DirectLayoutOptions> = {}): DirectLayoutOptions => ({
  t: 1000,
  blurBackground: false,
  failed: false,
  ...o,
});

describe('directLayout', () => {
  it('is eligible for the default fit (contain)', () => {
    expect(directLayout(layer(), 1920, 1080, 1920, 1080, opts())).toEqual({ fit: 'contain' });
    expect(directLayout(layer(), 1920, 1080, 1280, 1024, opts())).toEqual({ fit: 'contain' });
  });

  it('maps fill to cover and stretch to fill', () => {
    const fill = layer({ transform: { ...defaultTransform(), fit: 'fill' } });
    const stretch = layer({ transform: { ...defaultTransform(), fit: 'stretch' } });
    expect(directLayout(fill, 1920, 1080, 1000, 1000, opts())).toEqual({ fit: 'cover' });
    expect(directLayout(stretch, 1920, 1080, 1000, 1000, opts())).toEqual({ fit: 'fill' });
  });

  it('accepts custom placement only at scale 1 and the centre', () => {
    const ok = layer({ transform: { ...defaultTransform(), fit: 'custom', scale: 1, pos_x: 0, pos_y: 0 } });
    const moved = layer({ transform: { ...defaultTransform(), fit: 'custom', scale: 1, pos_x: 0.1, pos_y: 0 } });
    const scaled = layer({ transform: { ...defaultTransform(), fit: 'custom', scale: 0.5, pos_x: 0, pos_y: 0 } });
    expect(directLayout(ok, 1920, 1080, 1920, 1080, opts())).toEqual({ fit: 'contain' });
    expect(directLayout(moved, 1920, 1080, 1920, 1080, opts())).toBeNull();
    expect(directLayout(scaled, 1920, 1080, 1920, 1080, opts())).toBeNull();
  });

  it.each([
    ['rotated', { transform: { ...defaultTransform(), rotate_90: 1 } }],
    ['flipped h', { transform: { ...defaultTransform(), flip_h: true } }],
    ['flipped v', { transform: { ...defaultTransform(), flip_v: true } }],
    [
      'cropped',
      { transform: { ...defaultTransform(), crop: { left: 0.1, right: 0, top: 0, bottom: 0 } } },
    ],
    ['translucent', { opacity: 0.5 }],
    ['image', { kind: 'image' as const }],
    ['title', { kind: 'title' as const }],
    ['solid', { kind: 'solid' as const, media_id: null }],
    ['no media', { media_id: null }],
    [
      'in a transition',
      { transition: { kind: 'crossfade' as const, duration: 5000 }, clip_start: 0 },
    ],
  ])('is ineligible when %s', (_name, over) => {
    expect(directLayout(layer(over as Partial<VideoLayer>), 1920, 1080, 1920, 1080, opts())).toBeNull();
  });

  it('allows a transition that is already over', () => {
    const l = layer({ transition: { kind: 'crossfade', duration: 500 }, clip_start: 0 });
    expect(directLayout(l, 1920, 1080, 1920, 1080, opts({ t: 10_000 }))).toEqual({ fit: 'contain' });
  });

  it('is ineligible for failed media', () => {
    expect(directLayout(layer(), 1920, 1080, 1920, 1080, opts({ failed: true }))).toBeNull();
  });

  it('is ineligible for a blurred backdrop only when bars show', () => {
    expect(directLayout(layer(), 1920, 1080, 1080, 1920, opts({ blurBackground: true }))).toBeNull();
    expect(directLayout(layer(), 1920, 1080, 1280, 720, opts({ blurBackground: true }))).toEqual({ fit: 'contain' });
    const fill = layer({ transform: { ...defaultTransform(), fit: 'fill' } });
    expect(directLayout(fill, 1920, 1080, 1080, 1920, opts({ blurBackground: true }))).toEqual({ fit: 'cover' });
  });

  it('rejects unusable sizes', () => {
    expect(directLayout(layer(), 1920, 1080, 0, 1080, opts())).toBeNull();
    expect(directLayout(layer(), 0, 1080, 1920, 1080, opts())).toBeNull();
    expect(directLayout(layer(), 1920, 1080, Number.NaN, 1080, opts())).toBeNull();
  });
});

function state(over: Partial<DirectState> = {}): DirectState {
  return {
    enabled: true,
    playing: true,
    rate: 1,
    scrubbing: false,
    override: false,
    eligible: true,
    failed: false,
    elementPlaying: true,
    readyState: 4,
    seeking: false,
    framesSinceArm: 1,
    canvasPainted: true,
    ...over,
  };
}

describe('shouldEnterDirect / shouldLeaveDirect', () => {
  it('enters when playing, eligible and a frame was presented', () => {
    expect(shouldEnterDirect(state())).toBe(true);
    expect(shouldLeaveDirect(state())).toBe(false);
  });

  it.each([
    ['disabled', { enabled: false }],
    ['paused', { playing: false }],
    ['fast rate', { rate: 2 }],
    ['reverse', { rate: -1 }],
    ['scrubbing', { scrubbing: true }],
    ['override', { override: true }],
    ['ineligible', { eligible: false }],
    ['failed', { failed: true }],
    ['element paused', { elementPlaying: false }],
    ['not ready', { readyState: 1 }],
  ])('blocks entering and leaves when %s', (_n, over) => {
    expect(shouldEnterDirect(state(over))).toBe(false);
    expect(shouldLeaveDirect(state(over))).toBe(true);
    expect(directAllowed(state(over))).toBe(false);
  });

  it('waits for a presented frame, a settled element and a painted canvas, but does not leave for them', () => {
    for (const over of [{ framesSinceArm: 0 }, { seeking: true }, { canvasPainted: false }]) {
      expect(shouldEnterDirect(state(over))).toBe(false);
      expect(shouldLeaveDirect(state(over))).toBe(false);
    }
  });
});
