/**
 * Typed wrappers around the app-wide cache commands. Errors are rejected as strings and are not
 * caught here; the caller shows them.
 *
 * Only regenerable cache (audio extracts, thumbnails, waveforms) is counted and cleared. Recordings,
 * projects and the live recording buffer are never touched by these commands.
 */
import { invoke } from '@tauri-apps/api/core';

/** Cache size in bytes, from `app_cache_info` and `app_clear_cache`. Mirrors the Rust `AppCacheInfo`. */
export interface AppCacheInfo {
  /** The cache folder on disk. */
  dir: string;
  playerBytes: number;
  editorBytes: number;
  /** Old recording-buffer folders left by crashed sessions; the live buffer is never counted. */
  bufferBytes: number;
  /** Sum of the three sizes above. */
  totalBytes: number;
}

/** Size of the app cache now. */
export function cacheInfo(): Promise<AppCacheInfo> {
  return invoke<AppCacheInfo>('app_cache_info');
}

/** Deletes the regenerable cache and returns the size left afterwards. Rejects while an export runs. */
export function clearCache(): Promise<AppCacheInfo> {
  return invoke<AppCacheInfo>('app_clear_cache');
}
