// Filmstrip frame extraction for the timeline. Contract: docs/impl/player-v2.md. Browser only (DOM).
//
// Frames are taken with a detached, muted <video> so the visible player is never touched.
// Seeks run one at a time; a seek that errors or stalls is skipped rather than failing the strip.

/** Longest wait for one seek to finish before the frame is skipped. */
const SEEK_TIMEOUT_MS = 4000;
/** JPEG quality for each frame. */
const JPEG_QUALITY = 0.6;

/**
 * Times (seconds) of the frame centres, evenly spread over the clip:
 * ((i + 0.5) / count) * duration for i in 0..count-1. Empty for count <= 0 or an unknown duration.
 */
export function filmstripSeekTimes(count: number, durationS: number): number[] {
  const n = Math.floor(count);
  if (!(n > 0) || !Number.isFinite(durationS) || !(durationS > 0)) {
    return [];
  }
  const times: number[] = [];
  for (let i = 0; i < n; i += 1) {
    times.push(((i + 0.5) / n) * durationS);
  }
  return times;
}

/**
 * Resolves true when `event` fires on the video, false on 'error', abort,
 * or (when given) timeout. Listeners are always removed.
 */
function waitFor(
  video: HTMLVideoElement,
  event: 'loadedmetadata' | 'seeked',
  signal: AbortSignal,
  timeoutMs?: number,
): Promise<boolean> {
  return new Promise((resolve) => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const done = (ok: boolean): void => {
      video.removeEventListener(event, onOk);
      video.removeEventListener('error', onFail);
      signal.removeEventListener('abort', onFail);
      if (timer !== undefined) {
        clearTimeout(timer);
      }
      resolve(ok);
    };
    const onOk = (): void => done(true);
    const onFail = (): void => done(false);
    video.addEventListener(event, onOk);
    video.addEventListener('error', onFail);
    signal.addEventListener('abort', onFail);
    if (timeoutMs !== undefined) {
      timer = setTimeout(onFail, timeoutMs);
    }
  });
}

/**
 * Yields JPEG data URLs for `count` evenly spaced frames, `height` px tall, in
 * playback order. Ends early on abort or when the video cannot be read. Never throws.
 */
export async function* makeFilmstrip(
  videoUrl: string,
  count: number,
  height: number,
  signal: AbortSignal,
): AsyncGenerator<{ index: number; dataUrl: string }> {
  const video = document.createElement('video');
  try {
    if (signal.aborted || !(count > 0) || !(height > 0)) {
      return;
    }
    video.muted = true;
    video.preload = 'auto';
    video.crossOrigin = 'anonymous';
    video.src = videoUrl;
    const loaded = await waitFor(video, 'loadedmetadata', signal);
    if (!loaded || signal.aborted) {
      return;
    }
    const duration = video.duration;
    const vw = video.videoWidth;
    const vh = video.videoHeight;
    if (!Number.isFinite(duration) || !(duration > 0) || !(vw > 0) || !(vh > 0)) {
      return;
    }
    const width = Math.max(1, Math.round((height * vw) / vh));
    const canvas = document.createElement('canvas');
    canvas.width = width;
    canvas.height = height;
    const ctx = canvas.getContext('2d');
    if (!ctx) {
      return;
    }
    const times = filmstripSeekTimes(count, duration);
    for (let i = 0; i < times.length; i += 1) {
      if (signal.aborted) {
        return;
      }
      const seeked = waitFor(video, 'seeked', signal, SEEK_TIMEOUT_MS);
      video.currentTime = times[i];
      const ok = await seeked;
      if (signal.aborted) {
        return;
      }
      if (!ok) {
        continue;
      }
      ctx.drawImage(video, 0, 0, width, height);
      yield { index: i, dataUrl: canvas.toDataURL('image/jpeg', JPEG_QUALITY) };
    }
  } catch {
    // Any failure (tainted canvas, decode error) just ends the strip.
    return;
  } finally {
    video.removeAttribute('src');
    video.load();
  }
}
