/**
 * Helpers for the Video settings tab that the shared settings rules in pure.ts do not cover.
 * No Svelte and no I/O, so the tab stays a view over these functions.
 */

/**
 * Keeps the first entry for each GPU name. The daemon's adapter list can repeat one name (the
 * probe cache showed "NVIDIA GeForce RTX 5080" twice). The Selected GPU picker and the hardware
 * lines key their rows by name, and Svelte throws on a duplicate key, so duplicates must go
 * before they reach the template.
 */
export function uniqueGpus<T extends { name: string }>(gpus: readonly T[]): T[] {
  const seen = new Set<string>();
  return gpus.filter((g) => {
    if (seen.has(g.name)) return false;
    seen.add(g.name);
    return true;
  });
}

/**
 * The `[video]` values a fresh config has (schema.rs `Video::default`). Used by the error
 * boundary's "reset video settings" action. `container` is fixed at mp4 and not written here.
 */
export const VIDEO_DEFAULTS: Readonly<Record<string, string | number | boolean>> = {
  encoder: 'auto',
  codec: 'h264',
  preset: 'high',
  resolution: '1080p',
  fps: 60,
  rate_control: 'quality',
  quality_cq: 23,
  bitrate_kbps: 14000,
  max_bitrate_kbps: 15000,
  keyframe_interval_s: 2,
  cursor: true,
  gpu: 'auto',
  encoder_speed: 'balanced',
  b_frames: 2,
};

/** The dotted draft paths and values that restore the Video section to its defaults. */
export function videoDefaultWrites(): [string, string | number | boolean][] {
  return Object.entries(VIDEO_DEFAULTS).map(([key, value]) => [`video.${key}`, value]);
}
