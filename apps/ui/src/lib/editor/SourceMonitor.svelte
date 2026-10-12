<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { get } from 'svelte/store';
  import { editorApi } from './api';
  import { sourceRequest } from './actions';
  import { dispatch, playhead, project, timelineEndTick, toast } from './store';
  import { formatDuration, formatTimecode, secToTick, tickToSec } from './ticks';
  import type { PlaceMode, Rate, Tick } from './types';

  interface Props {
    /** Media file to show, or null when nothing is chosen. */
    mediaId: string | null;
    onclose: () => void;
  }

  let { mediaId, onclose }: Props = $props();

  /** Bar heights (percent) for the audio placeholder. Fixed values keep the look stable. */
  const WAVE_BARS: number[] = Array.from(
    { length: 96 },
    (_, i) => 18 + Math.round(Math.abs(Math.sin(i * 0.37) * Math.cos(i * 0.11)) * 70),
  );
  /** Frame step used by the arrow keys when the media rate is unknown. */
  const FALLBACK_FPS = 30;
  /** Shown when a video element reports no picture size (WebView2 without the HEVC extension). */
  const CODEC_MESSAGE = 'This video codec cannot be decoded by WebView2 (HEVC?)';
  /** Keys this monitor handles. Other keys keep their global meaning. */
  const HANDLED_KEYS = new Set(['i', 'I', 'o', 'O', ' ', 'k', 'K', 'ArrowLeft', 'ArrowRight']);

  /** Object URL from the media server, or '' while loading. */
  let src = $state('');
  /** Error while fetching the media URL. */
  let loadError = $state<string | null>(null);
  /** Error reported by the media element (for example an undecodable codec). */
  let playbackError = $state<string | null>(null);
  let videoEl = $state<HTMLVideoElement | undefined>(undefined);
  let audioEl = $state<HTMLAudioElement | undefined>(undefined);
  /** Current position in seconds. Bound to the media element. */
  let currentSec = $state(0);
  /** Duration in seconds as reported by the element (NaN or Infinity until known). */
  let durationSec = $state(Number.NaN);
  let paused = $state(true);
  /** In point and out point in source ticks. Null means not set. */
  let inTick = $state<Tick | null>(null);
  let outTick = $state<Tick | null>(null);

  const media = $derived($project?.media.find((m) => m.id === mediaId) ?? null);
  const isVideo = $derived(media?.kind === 'video');
  const isAudio = $derived(media?.kind === 'audio');
  const isImage = $derived(media?.kind === 'image');
  /** Length in seconds: the element's duration when known, else the media record. */
  const totalSec = $derived(
    Number.isFinite(durationSec) && durationSec > 0
      ? durationSec
      : tickToSec(media?.duration ?? 0),
  );
  /** Timecode rate: the media frame rate when known, else the project rate. */
  const rate = $derived<Rate>(
    media !== null && Number.isFinite(media.fps) && media.fps > 0
      ? { num: Math.round(media.fps * 1000), den: 1000 }
      : ($project?.canvas.fps ?? { num: FALLBACK_FPS, den: 1 }),
  );
  const frameSec = $derived(rate.den / rate.num);
  const inSec = $derived(inTick === null ? null : tickToSec(inTick));
  const outSec = $derived(outTick === null ? null : tickToSec(outTick));
  const pct = (sec: number): number => (totalSec > 0 ? Math.min(100, (sec / totalSec) * 100) : 0);
  /** Left and width (percent) of the marked range on the scrub bar, or null. */
  const rangeStyle = $derived.by((): string | null => {
    if (inSec === null && outSec === null) return null;
    const a = inSec ?? 0;
    const b = outSec ?? totalSec;
    const left = pct(a);
    const width = Math.max(0, pct(b) - left);
    return `left: ${left}%; width: ${width}%;`;
  });

  /** The element that plays the current media (null for images). */
  function mediaEl(): HTMLMediaElement | undefined {
    if (isAudio) return audioEl;
    if (isVideo) return videoEl;
    return undefined;
  }

  /** Clears transport state and marks, then fetches the URL of the new media. */
  $effect(() => {
    const id = mediaId;
    untrack(() => {
      src = '';
      loadError = null;
      playbackError = null;
      inTick = null;
      outTick = null;
      currentSec = 0;
      durationSec = Number.NaN;
      paused = true;
      mediaEl()?.pause();
    });
    if (id === null) return;
    let cancelled = false;
    editorApi.mediaUrl(id).then(
      (u) => {
        if (!cancelled) src = u;
      },
      (e: unknown) => {
        if (!cancelled) loadError = String(e);
      },
    );
    return () => {
      cancelled = true;
    };
  });

  /** Request already pending at mount. It is stale, so it is consumed without being performed. */
  const mountRequest = get(sourceRequest);

  /** Handles a `,` or `.` request from the editor actions. Each request is consumed once. */
  $effect(() => {
    const req = $sourceRequest;
    if (req === null) return;
    sourceRequest.set(null);
    if (req === mountRequest) return;
    untrack(() => place(req.action === 'insert' ? 'insert' : 'overwrite'));
  });

  onDestroy(() => {
    videoEl?.pause();
    audioEl?.pause();
  });

  /** Toggles playback of the media element. */
  function togglePlay(): void {
    const el = mediaEl();
    if (el === undefined) return;
    if (el.paused) {
      playbackError = null;
      el.play().catch((e: unknown) => {
        if (e instanceof DOMException && e.name === 'AbortError') return;
        playbackError = `Playback failed: ${String(e)}`;
      });
    } else {
      el.pause();
    }
  }

  /** Moves to `sec`, clamped to the media length. Images only update the readout. */
  function seekSec(sec: number): void {
    if (!Number.isFinite(sec)) return;
    const max = totalSec > 0 ? totalSec : 0;
    const next = Math.min(max, Math.max(0, sec));
    const el = mediaEl();
    if (el !== undefined) el.currentTime = next;
    currentSec = next;
  }

  /** Steps the position by `deltaSec` (negative = back). */
  function step(deltaSec: number): void {
    seekSec(currentSec + deltaSec);
  }

  /** Sets the in point at the current position. An out point at or before it is cleared. */
  function setIn(): void {
    const t = secToTick(currentSec);
    inTick = t;
    if (outTick !== null && outTick <= t) outTick = null;
  }

  /** Sets the out point at the current position. Ignored when not after the in point. */
  function setOut(): void {
    const t = secToTick(currentSec);
    const base = inTick ?? 0;
    if (t <= base) {
      toast.set('The out point must come after the in point.');
      return;
    }
    outTick = t;
  }

  /** Clears both marks. */
  function clearMarks(): void {
    inTick = null;
    outTick = null;
  }

  /**
   * Places the marked range on the timeline. Insert and overwrite start at the playhead; append
   * goes after the last clip. Without an out point the range runs to the end of the media.
   */
  function place(mode: PlaceMode): void {
    if (mediaId === null || media === null) {
      toast.set('Open a media file in the source monitor first.');
      return;
    }
    const srcIn: Tick = inTick ?? 0;
    const srcLen: Tick = outTick !== null ? Math.max(0, outTick - srcIn) : 0;
    const at: Tick = mode === 'append' ? timelineEndTick(get(project)) : get(playhead);
    void dispatch({
      type: 'place_media',
      media_id: mediaId,
      at,
      src_in: srcIn,
      src_len: srcLen,
      video_track: null,
      mode,
    });
  }

  /** Keyboard handling inside the monitor. Keys are only active when the monitor has focus. */
  function onKeydown(e: KeyboardEvent): void {
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    const target = e.target;
    // Leave typing and sliders alone except for the marking keys.
    const inField = target instanceof HTMLInputElement && target.type !== 'range';
    if (inField) return;
    if (!HANDLED_KEYS.has(e.key)) return;
    // Handled keys must not also fire the global shortcuts.
    e.preventDefault();
    e.stopPropagation();
    switch (e.key) {
      case 'i':
      case 'I':
        setIn();
        break;
      case 'o':
      case 'O':
        setOut();
        break;
      case ' ':
      case 'k':
      case 'K':
        togglePlay();
        break;
      case 'ArrowLeft':
        step(-(e.shiftKey ? 1 : frameSec));
        break;
      case 'ArrowRight':
        step(e.shiftKey ? 1 : frameSec);
        break;
      default:
        break;
    }
  }

  /** Error text from the media element's MediaError. */
  function onMediaError(e: Event): void {
    const el = e.currentTarget;
    if (!(el instanceof HTMLMediaElement) || src === '') return;
    const code = el.error?.code;
    playbackError =
      code === 4
        ? 'This file cannot be previewed here. The codec is probably not supported by WebView2 (HEVC is a common case). The edit still works.'
        : 'This file could not be played in the preview.';
  }

  /** A video with no picture size after its metadata loads cannot be decoded here. */
  function onVideoMetadata(e: Event): void {
    const el = e.currentTarget;
    if (!(el instanceof HTMLVideoElement)) return;
    if (el.videoWidth === 0) playbackError = CODEC_MESSAGE;
  }

</script>

<!-- Focusable region with its own key handling (I, O, Space, arrows), by design. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div
  class="source-monitor"
  role="application"
  aria-label="Source monitor. Keys: I mark in, O mark out, Space play or pause, arrows step frames."
  tabindex="0"
  onkeydown={onKeydown}
>
  <header class="head">
    <div class="title">
      <span class="label">Source</span>
      {#if media !== null}
        <span class="name" title={media.name}>{media.name}</span>
        <span class="dur">{formatDuration(media.duration)}</span>
      {:else}
        <span class="name dim">No media selected</span>
      {/if}
    </div>
    <button type="button" class="icon-btn" aria-label="Close source monitor" onclick={onclose}>
      <svg width="14" height="14" viewBox="0 0 24 24" aria-hidden="true"
        ><path
          d="M6 6l12 12M18 6L6 18"
          fill="none"
          stroke="currentColor"
          stroke-width="2"
          stroke-linecap="round"
        /></svg
      >
    </button>
  </header>

  {#if mediaId === null}
    <div class="viewer empty">
      <p>Choose a file in the media bin to load it here.</p>
    </div>
  {:else}
    <div class="viewer">
      {#if loadError !== null}
        <p class="message error" role="alert">Could not load this file: {loadError}</p>
      {:else if media?.missing}
        <p class="message error" role="alert">
          The file is missing. Relink it from the media bin.
        </p>
      {:else if isVideo}
        <!-- svelte-ignore a11y_media_has_caption -->
        <video
          class="picture"
          bind:this={videoEl}
          src={src || undefined}
          preload="auto"
          aria-label={media?.name ?? 'Source video'}
          bind:currentTime={currentSec}
          bind:duration={durationSec}
          bind:paused
          onerror={onMediaError}
          onloadedmetadata={onVideoMetadata}
        ></video>
      {:else if isAudio}
        <audio
          bind:this={audioEl}
          src={src || undefined}
          preload="auto"
          aria-label={media?.name ?? 'Source audio'}
          bind:currentTime={currentSec}
          bind:duration={durationSec}
          bind:paused
          onerror={onMediaError}
        ></audio>
        <div class="wave" aria-hidden="true">
          {#each WAVE_BARS as h, i (i)}
            <span style="height: {h}%"></span>
          {/each}
        </div>
      {:else if isImage && src !== ''}
        <img class="picture" src={src} alt={media?.name ?? 'Source image'} />
      {:else}
        <p class="message">Loading...</p>
      {/if}

      {#if playbackError !== null}
        <p class="message error" role="alert">
          {playbackError}
          <button type="button" class="link-btn" onclick={() => (playbackError = null)}>
            Dismiss
          </button>
        </p>
      {/if}
    </div>

    <div class="transport">
      <div class="scrub">
        <div class="track">
          {#if rangeStyle !== null}
            <div class="range" style={rangeStyle}></div>
          {/if}
          {#if inSec !== null}
            <div class="mark in" style="left: {pct(inSec)}%" title="In point"></div>
          {/if}
          {#if outSec !== null}
            <div class="mark out" style="left: {pct(outSec)}%" title="Out point"></div>
          {/if}
        </div>
        <input
          type="range"
          class="slider"
          min="0"
          max={totalSec > 0 ? totalSec : 0}
          step="any"
          value={currentSec}
          disabled={totalSec <= 0 || isImage}
          aria-label="Scrub position"
          aria-valuetext={formatTimecode(secToTick(currentSec), rate)}
          oninput={(e) => seekSec(Number(e.currentTarget.value))}
        />
      </div>

      <div class="row">
        <button
          type="button"
          class="btn"
          aria-label={paused ? 'Play source' : 'Pause source'}
          aria-keyshortcuts="Space K"
          disabled={isImage || mediaId === null}
          onclick={togglePlay}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" aria-hidden="true">
            {#if paused}
              <path d="M7 4l13 8-13 8z" fill="currentColor" />
            {:else}
              <path d="M6 4h4v16H6zM14 4h4v16h-4z" fill="currentColor" />
            {/if}
          </svg>
        </button>
        <button
          type="button"
          class="btn"
          aria-label="Step back one frame"
          disabled={isImage || mediaId === null}
          onclick={() => step(-frameSec)}>&#x2039;</button
        >
        <button
          type="button"
          class="btn"
          aria-label="Step forward one frame"
          disabled={isImage || mediaId === null}
          onclick={() => step(frameSec)}>&#x203A;</button
        >
        <span class="timecode" aria-label="Position">
          {formatTimecode(secToTick(currentSec), rate)}
        </span>
        <span class="sep" aria-hidden="true">/</span>
        <span class="timecode dim" aria-label="Duration">
          {formatTimecode(secToTick(totalSec), rate)}
        </span>
      </div>

      <div class="row">
        <button
          type="button"
          class="btn"
          aria-label="Mark in point (I)"
          title="Mark in (I)"
          disabled={mediaId === null}
          onclick={setIn}>Mark in</button
        >
        <button
          type="button"
          class="btn"
          aria-label="Mark out point (O)"
          title="Mark out (O)"
          disabled={mediaId === null}
          onclick={setOut}>Mark out</button
        >
        <button
          type="button"
          class="btn"
          aria-label="Clear in and out points"
          disabled={inTick === null && outTick === null}
          onclick={clearMarks}>Clear</button
        >
        <span class="marks-text dim">
          In {inSec === null ? '--' : formatTimecode(inTick ?? 0, rate)}
          &middot; Out {outSec === null ? '--' : formatTimecode(outTick ?? 0, rate)}
        </span>
      </div>

      <div class="row place">
        <button
          type="button"
          class="btn primary"
          aria-label="Insert at playhead"
          title="Insert (,)"
          disabled={mediaId === null}
          onclick={() => place('insert')}>Insert</button
        >
        <button
          type="button"
          class="btn"
          aria-label="Overwrite at playhead"
          title="Overwrite (.)"
          disabled={mediaId === null}
          onclick={() => place('overwrite')}>Overwrite</button
        >
        <button
          type="button"
          class="btn"
          aria-label="Append to the end of the timeline"
          disabled={mediaId === null}
          onclick={() => place('append')}>Append</button
        >
      </div>
    </div>
  {/if}
</div>

<style>
  .source-monitor {
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-width: 0;
    min-height: 0;
    height: 100%;
    padding: 8px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text);
    outline: none;
  }

  .source-monitor:focus-visible {
    box-shadow: inset 0 0 0 2px var(--accent);
  }

  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    min-width: 0;
  }

  .title {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }

  .label {
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-dim);
  }

  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }

  .dim {
    color: var(--text-dim);
  }

  .dur {
    color: var(--text-dim);
    font-variant-numeric: tabular-nums;
  }

  .viewer {
    position: relative;
    flex: 1 1 auto;
    min-height: 120px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: #000;
    border-radius: var(--radius);
    overflow: hidden;
  }

  .viewer.empty {
    background: var(--surface-2);
    color: var(--text-dim);
    padding: 16px;
    text-align: center;
  }

  .picture {
    max-width: 100%;
    max-height: 100%;
    width: 100%;
    height: 100%;
    object-fit: contain;
  }

  audio {
    position: absolute;
    width: 0;
    height: 0;
    opacity: 0;
  }

  .wave {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 2px;
    width: 92%;
    height: 60%;
  }

  .wave span {
    flex: 1 1 0;
    min-width: 1px;
    background: var(--text-dim);
    border-radius: 1px;
    opacity: 0.7;
  }

  .message {
    margin: 0;
    padding: 12px;
    color: var(--text-dim);
    text-align: center;
    max-width: 90%;
  }

  .message.error {
    color: var(--danger);
  }

  .link-btn {
    margin-left: 8px;
    background: none;
    border: none;
    color: var(--text);
    text-decoration: underline;
    cursor: pointer;
    padding: 0;
  }

  .transport {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .scrub {
    position: relative;
    height: 22px;
  }

  .track {
    position: absolute;
    left: 0;
    right: 0;
    top: 9px;
    height: 4px;
    background: var(--surface-2);
    border-radius: 2px;
    pointer-events: none;
  }

  .range {
    position: absolute;
    top: 0;
    bottom: 0;
    background: var(--accent);
    opacity: 0.45;
    border-radius: 2px;
  }

  .mark {
    position: absolute;
    top: -5px;
    width: 2px;
    height: 14px;
    margin-left: -1px;
    background: var(--text);
  }

  .slider {
    position: absolute;
    inset: 0;
    width: 100%;
    margin: 0;
    background: transparent;
    accent-color: var(--accent);
  }

  .row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px;
    min-width: 0;
  }

  .timecode {
    font-variant-numeric: tabular-nums;
    font-family: ui-monospace, Consolas, monospace;
    font-size: 13px;
  }

  .sep {
    color: var(--text-dim);
  }

  .marks-text {
    font-size: 12px;
    font-variant-numeric: tabular-nums;
  }

  .btn {
    min-height: 28px;
    padding: 0 10px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: var(--surface-2);
    color: var(--text);
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
    font-size: 13px;
  }

  .btn:hover:not(:disabled) {
    border-color: var(--text-dim);
  }

  .btn.primary {
    border-color: var(--accent);
  }

  .btn:focus-visible,
  .icon-btn:focus-visible,
  .link-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .btn:disabled {
    opacity: 0.45;
    cursor: default;
  }

  .icon-btn {
    width: 28px;
    height: 28px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: transparent;
    border: none;
    border-radius: 6px;
    color: var(--text-dim);
    cursor: pointer;
    flex: 0 0 auto;
  }

  .icon-btn:hover {
    color: var(--text);
    background: var(--surface-2);
  }
</style>
