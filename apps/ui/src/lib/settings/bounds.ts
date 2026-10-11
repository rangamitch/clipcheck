/**
 * Limits the Settings screen enforces in its inputs. Each value mirrors a range the daemon
 * checks in crates/core/src/config/validate.rs. When a limit changes there, change it here too.
 * Nothing else in the UI should type these numbers by hand.
 */

/** `replay.max_seconds`: the buffer length (validate.rs, replay check, 30..=3600 s). */
export const BUFFER_MIN_SECONDS = 30;
export const BUFFER_MAX_SECONDS = 3600;

/** `replay.ram_max_mb`: the RAM budget for the buffer (validate.rs, 64..=4096 MB). */
export const RAM_MAX_MB_MIN = 64;
export const RAM_MAX_MB_MAX = 4096;

/** Clip hotkey length in seconds (clips.ts documents 5..=3600). */
export const CLIP_MIN_SECONDS = 5;
export const CLIP_MAX_SECONDS = 3600;

/** Clip hotkey and track or app names, in Unicode characters (validate.rs counts `chars()`). */
export const CLIP_NAME_MAX_CHARS = 32;
export const APP_NAME_MAX_CHARS = 32;

/** `[[hotkeys.clips]]` entries (schema limit). */
export const MAX_CLIPS = 8;

/** `storage.quota_gb`: 0 turns the quota off, otherwise 1..=100000 GB (validate.rs). */
export const QUOTA_GB_MAX = 100000;

/** `video.bitrate_kbps` and `video.max_bitrate_kbps` (CFG-V005, 1000..=100000 kbps). */
export const BITRATE_MIN_KBPS = 1000;
export const BITRATE_MAX_KBPS = 100000;

/** `audio.bitrate_kbps` (validate.rs audio check, 128..=320 kbps). */
export const AAC_BITRATE_MIN_KBPS = 128;
export const AAC_BITRATE_MAX_KBPS = 320;

/** `detection.stop_grace_s` (validate.rs, 0..=300 s). */
export const STOP_GRACE_MIN_S = 0;
export const STOP_GRACE_MAX_S = 300;
