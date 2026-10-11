<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from 'svelte';
  import {
    getGameIcons,
    getMediaBase,
    getRecordingDetails,
    mediaUrl,
    openPlayer,
    prepareAudio,
    type AudioTrackDto,
    type PlayerOpenDto,
    type RecordingDto,
  } from './api';
  import { openRecording, playerRow, updatePlayerRow } from './nav';
  import {
    isMuted,
    loadVolume,
    saveVolume,
    setLevel,
    toggleMute,
    type VolumeState,
  } from './player/volumeprefs';
  import {
    deleteRecording,
    renameRecording,
    revealRecording,
    setFavorite,
  } from './recordingActions';
  import {
    cancelTrim,
    getTrimPreview,
    onTrimDone,
    onTrimProgress,
    startTrim,
    type TrimMode,
  } from './trimapi';
  import { thumbUrl } from './thumbs';
  import {
    clampTime,
    clampVolume,
    initTracks,
    keyToAction,
    percentJump,
    stepSpeed,
    soloTrack,
    stepVolume,
    toggleTrack,
    type PlayerAction,
    type TrackState,
  } from './player/model';
  import { TrackMixer, syncAudioToVideo } from './player/mixer';
  import { clampSelection, setIn, setOut, snapReadout, type TrimSelection } from './player/trim';
  import { confinePlayback, filmstripCount, isTrimmed } from './player/timeline';
  import { makeFilmstrip } from './player/filmstrip';
  import AudioMenu from './player/AudioMenu.svelte';
  import Controls from './player/Controls.svelte';
  import DetailsPanel from './player/DetailsPanel.svelte';
  import Icon from './player/Icon.svelte';
  import Timeline from './player/Timeline.svelte';
  import RecordingMenu from './RecordingMenu.svelte';

  interface Props {
    id: string;
    /** Full file name with extension, shown in the header. Empty when the caller does not know it. */
    fileName?: string;
    onclose: () => void;
  }

  let { id, fileName = '', onclose }: Props = $props();

  const FRAME_S = 1 / 30;
  const SYNC_INTERVAL_MS = 250;
  const CLICK_DELAY_MS = 200;
  const SPINNER_DELAY_MS = 400;
  const FILM_HEIGHT = 56;
  const FILM_START_DELAY_MS = 2500;
  const FILM_MAX_FRAMES = 24;
  const PREVIEW_DEBOUNCE_MS = 200;

  /** Video URL on the loopback media server (clip.localhost fallback). '' until the base is known. */
  let videoSrc = $state('');
  /** Media base from getMediaBase, or null for the fallback. Set before any audio is attached. */
  let mediaBaseUrl: string | null = null;
  /** Delayed filmstrip start (2500 ms after the first playing event). */
  let filmTimer: ReturnType<typeof setTimeout> | null = null;

  let dto = $state<PlayerOpenDto | null>(null);
  let video = $state<HTMLVideoElement | undefined>(undefined);
  let root = $state<HTMLDivElement | undefined>(undefined);
  let mainEl = $state<HTMLDivElement | undefined>(undefined);
  let menuRef = $state<{ showAt: (x: number, y: number) => void } | undefined>(undefined);
  /** Row patch used when the player was opened by id only (the nav store has no row to update). */
  let localPatch = $state<Partial<RecordingDto>>({});

  /** True when the Web Audio mixer drives the sound (video muted, one hidden audio per track). */
  let mixedActive = $state(false);
  /** Track list backing the mixer (sidecar urls); empty until the background prep resolves. */
  let trackInfos = $state<AudioTrackDto[]>([]);
  /** Dismissible one-line note: backend audioError or a mixer setup failure. */
  let audioNotice = $state<string | null>(null);
  let videoError = $state(false);
  let needsPlayButton = $state(false);
  let showSpinner = $state(false);
  let hasPlayed = false;

  let tracks = $state<TrackState[]>([]);
  // Volume is shared by all clips and remembered across sessions; muting is volume 0 (YouTube-style).
  let volState = $state<VolumeState>(loadVolume());
  const volume = $derived(volState.volume);
  const muted = $derived(isMuted(volState));
  let now = $state(0);
  let duration = $state(0);
  let rate = $state(1);
  let paused = $state(true);
  let isFullscreen = $state(false);

  let selection = $state<TrimSelection>({ startMs: 0, endMs: 0 });
  let lastFullMs = 0;
  let frames = $state<string[]>([]);
  let filmStarted = false;
  let filmAbort: AbortController | null = null;

  let path = $state<string | null>(null);
  let gameIcon = $state<string | undefined>(undefined);

  // Save Edits state.
  let savePopover = $state(false);
  let saveWrap = $state<HTMLDivElement | undefined>(undefined);
  let replaceOriginal = $state(false);
  let exactCut = $state(false);
  let snapText = $state('');
  let trimRunning = $state(false);
  let trimPercent = $state(0);
  let trimJobId: string | null = null;
  let trimError = $state('');
  let savedNewId = $state<string | null>(null);
  let previewSeq = 0;
  let previewTimer: ReturnType<typeof setTimeout> | null = null;
  let unlisteners: Array<() => void> = [];

  const mixer = new TrackMixer();
  let audios: HTMLAudioElement[] = [];
  let syncTimer: ReturnType<typeof setInterval> | null = null;
  let clickTimer: ReturnType<typeof setTimeout> | null = null;
  let spinnerTimer: ReturnType<typeof setTimeout> | null = null;
  let cancelled = false;

  function splitExtension(name: string): string {
    const dot = name.lastIndexOf('.');
    return dot > 0 ? name.slice(dot) : '';
  }

  /** The Gallery row, or a minimal one built from the file name and the open dto. */
  const row = $derived.by<RecordingDto>(() => {
    const base = $playerRow;
    if (base !== null) return base;
    const ext = splitExtension(fileName);
    return {
      id,
      kind: 'clip',
      sessionId: null,
      partIndex: null,
      gameId: null,
      gameName: null,
      fileName,
      ext: ext.startsWith('.') ? ext.slice(1) : ext,
      sizeBytes: 0,
      durationMs: dto?.durationMs ?? 0,
      createdAtMs: 0,
      status: 'ready',
      isFavorite: false,
      recovered: false,
      degraded: false,
      needsFaststart: false,
      ...localPatch,
    };
  });

  const displayName = $derived(row.fileName);
  const durationMs = $derived(Math.round(duration * 1000));
  const trimmed = $derived(isTrimmed(selection, durationMs));
  const showAudioMenu = $derived(mixedActive && trackInfos.length >= 2);
  const aspect = $derived(
    dto && dto.width > 0 && dto.height > 0 ? dto.width / dto.height : 16 / 9,
  );

  function errorText(err: unknown): string {
    if (err instanceof Error) return err.message;
    if (typeof err === 'string') return err;
    return 'Unknown error';
  }

  /** Merges a change into the open row (store when present, local patch otherwise). */
  function applyPatch(patch: Partial<RecordingDto>) {
    updatePlayerRow(patch);
    localPatch = { ...localPatch, ...patch };
  }

  /** Media duration in seconds: the element's own value once known, else the known one. */
  function mediaDuration(): number {
    if (video && Number.isFinite(video.duration) && video.duration > 0) return video.duration;
    return duration;
  }

  // ---- Audio mixer (background multi-track) ----

  /** Builds the hidden audio elements and hands playback sound to the mixer. */
  function activateMixer(infos: AudioTrackDto[]) {
    try {
      audios = infos.map((t) => {
        const a = new Audio();
        // CORS must be requested before the load starts, or Web Audio gets silence from the media server origin.
        a.crossOrigin = 'anonymous';
        a.preload = 'auto';
        a.src = t.url ? mediaUrl(mediaBaseUrl, 'aud', id, t.index) : '';
        return a;
      });
      mixer.attach(audios);
      trackInfos = infos;
      tracks = initTracks(infos.length);
      mixedActive = true;
      if (video) {
        video.muted = true;
        for (const a of audios) a.currentTime = video.currentTime;
        if (!video.paused) {
          mixer.resume().catch(() => {});
          startSyncTimer();
        }
        syncAudio();
      }
    } catch (err) {
      teardownAudio();
      mixedActive = false;
      audioNotice = errorText(err);
    }
  }

  function loadAudioInBackground(opened: PlayerOpenDto) {
    if (opened.mixed && opened.tracks.length >= 2 && opened.tracks.every((t) => t.url)) {
      activateMixer(opened.tracks);
      return;
    }
    if (!opened.audioPending) {
      audioNotice = opened.audioError;
      return;
    }
    prepareAudio(id).then(
      (res) => {
        if (cancelled) return;
        if (res.mixed && res.tracks.length >= 2) {
          activateMixer(res.tracks);
        } else if (res.audioError) {
          audioNotice = res.audioError;
        }
      },
      (err: unknown) => {
        if (!cancelled) audioNotice = errorText(err);
      },
    );
  }

  function teardownAudio() {
    stopSyncTimer();
    for (const a of audios) {
      a.pause();
      a.removeAttribute('src');
      a.load();
    }
    audios = [];
    mixer.detach();
  }

  function startSyncTimer() {
    if (syncTimer !== null) return;
    syncTimer = setInterval(() => {
      if (video && audios.length > 0) syncAudioToVideo(video, audios);
    }, SYNC_INTERVAL_MS);
  }

  function stopSyncTimer() {
    if (syncTimer !== null) {
      clearInterval(syncTimer);
      syncTimer = null;
    }
  }

  function syncAudio() {
    if (video && audios.length > 0) syncAudioToVideo(video, audios);
  }

  // ---- Playback ----

  function togglePlay() {
    if (!video) return;
    if (video.paused) {
      // At or past the kept range's end: restart from its start.
      if (trimmed && video.currentTime * 1000 >= selection.endMs - 50) {
        video.currentTime = selection.startMs / 1000;
      }
      mixer.resume().catch(() => {});
      video.play().catch(() => {});
    } else {
      video.pause();
    }
  }

  async function toggleFullscreen() {
    const target = mainEl ?? root;
    if (!target) return;
    try {
      if (document.fullscreenElement) {
        await document.exitFullscreen();
      } else {
        await target.requestFullscreen();
      }
    } catch {
      // Fullscreen refused by the webview: nothing to show.
    }
  }

  function setRate(r: number) {
    if (!video) return;
    video.playbackRate = r;
    rate = video.playbackRate;
  }

  // Seeks are coalesced: at most one in flight, latest target wins. A drag fires a seek per pointer
  // move; flooding the decoder with them freezes playback.
  let seekTarget: number | null = null;
  let seekFrame = 0;
  let seekWaiting = false;
  /** Bounds checks stay off until this time (performance.now) after a programmatic seek. */
  let seekGuardUntil = 0;

  function flushSeek(force = false) {
    if (seekFrame) {
      cancelAnimationFrame(seekFrame);
      seekFrame = 0;
    }
    if (!video || seekTarget === null) return;
    if (video.seeking && !force) {
      if (!seekWaiting) {
        seekWaiting = true;
        video.addEventListener(
          'seeked',
          () => {
            seekWaiting = false;
            flushSeek();
          },
          { once: true },
        );
      }
      return;
    }
    const t = seekTarget;
    seekTarget = null;
    seekGuardUntil = performance.now() + 300;
    video.currentTime = t;
  }

  function queueSeek(sec: number) {
    seekTarget = sec;
    now = sec;
    if (!seekFrame) seekFrame = requestAnimationFrame(() => flushSeek());
  }

  /** Seeks, keeping the playhead inside the SOS..EOS range. */
  function seekMs(ms: number) {
    if (!video) return;
    const lo = selection.startMs;
    const hi = selection.endMs > lo ? selection.endMs : durationMs;
    queueSeek(Math.min(Math.max(ms, lo), hi) / 1000);
  }

  /** True while the pointer is held on the timeline or a trim handle. */
  let scrubbing = false;

  /** Whether the video was playing when the current scrub began. */
  let resumeAfterScrub = false;

  /** Holding the timeline or a handle pauses the video; releasing resumes it if it was playing. */
  function onScrub(active: boolean) {
    scrubbing = active;
    if (active) {
      resumeAfterScrub = video !== undefined && !video.paused;
      video?.pause();
      return;
    }
    if (!resumeAfterScrub) return;
    resumeAfterScrub = false;
    // The release seek (snap, or the EOS preview) happens right after this call; resume after it.
    queueMicrotask(() => {
      if (scrubbing || !video) return;
      flushSeek(true);
      mixer.resume().catch(() => {});
      video.play().catch(() => {});
    });
  }

  /** Follows a held scrub anywhere in the clip (the release snaps back into SOS..EOS). */
  function seekFree(ms: number) {
    if (!video) return;
    queueSeek(Math.min(Math.max(ms, 0), durationMs) / 1000);
  }

  function onTimeUpdate() {
    if (!video) return;
    now = video.currentTime;
    // Bounds only apply to real playback: seeks to the EOS/SOS while dragging must not bounce back.
    if (scrubbing || video.paused || video.seeking || video.readyState < 2) return;
    if (performance.now() < seekGuardUntil) return;
    const c = confinePlayback(now * 1000, selection, durationMs);
    if (c.action === 'pause-and-seek') {
      video.pause();
      seekMs(c.toMs);
    }
  }

  function runAction(action: PlayerAction) {
    if (!video) return;
    const dur = mediaDuration();
    switch (action.type) {
      case 'toggle-play':
        togglePlay();
        break;
      case 'seek':
        seekMs(clampTime(video.currentTime + action.deltaS, dur) * 1000);
        break;
      case 'volume':
        volState = setLevel(volState, stepVolume(volState.volume, action.deltaPct));
        break;
      case 'mute':
        volState = toggleMute(volState);
        break;
      case 'fullscreen':
        void toggleFullscreen();
        break;
      case 'speed':
        setRate(stepSpeed(video.playbackRate, action.dir));
        break;
      case 'jump':
        seekMs(percentJump(action.digit, dur) * 1000);
        break;
      case 'track-toggle':
        if (action.index < tracks.length) {
          tracks = toggleTrack(tracks, action.index);
        }
        break;
      case 'track-solo':
        if (action.index < tracks.length) {
          tracks = soloTrack(tracks, action.index);
        }
        break;
      case 'frame':
        video.pause();
        seekMs(clampTime(video.currentTime + action.dir * FRAME_S, dur) * 1000);
        break;
    }
  }

  function isTextTarget(t: EventTarget | null): boolean {
    return (
      t instanceof HTMLInputElement ||
      t instanceof HTMLTextAreaElement ||
      (t instanceof HTMLElement && t.isContentEditable)
    );
  }

  function onKeydown(e: KeyboardEvent) {
    // Sliders, checkboxes and the name field keep their native keys.
    if (isTextTarget(e.target)) return;
    if (e.key === 'Escape' && savePopover) {
      e.preventDefault();
      savePopover = false;
      return;
    }
    if (dto && !dto.playable) return;
    if (!e.ctrlKey && !e.metaKey && !e.altKey && !e.shiftKey) {
      const k = e.key.toLowerCase();
      if (k === 'i' || k === 'o') {
        e.preventDefault();
        const ms = (video?.currentTime ?? 0) * 1000;
        selection =
          k === 'i' ? setIn(selection, ms, durationMs) : setOut(selection, ms, durationMs);
        return;
      }
    }
    const action = keyToAction(e);
    if (action === null) return;
    e.preventDefault();
    runAction(action);
  }

  function onVideoClick() {
    // Delay the play toggle so a double-click can cancel it and toggle fullscreen instead.
    if (clickTimer !== null) clearTimeout(clickTimer);
    clickTimer = setTimeout(() => {
      clickTimer = null;
      togglePlay();
    }, CLICK_DELAY_MS);
  }

  function onVideoDblClick() {
    if (clickTimer !== null) {
      clearTimeout(clickTimer);
      clickTimer = null;
    }
    void toggleFullscreen();
  }

  function onVideoContextMenu(e: MouseEvent) {
    e.preventDefault();
    menuRef?.showAt(e.clientX, e.clientY);
  }

  function onRecordingChanged(
    change:
      | { kind: 'renamed'; id: string; name: string }
      | { kind: 'deleted'; id: string }
      | { kind: 'favorite'; id: string; value: boolean },
  ) {
    if (change.kind === 'deleted') {
      onclose();
      return;
    }
    if (change.kind === 'favorite') {
      applyPatch({ isFavorite: change.value });
      return;
    }
    // The menu passes the stem only; keep the original extension on the full name.
    applyPatch({ fileName: change.name + splitExtension(displayName) });
  }

  function onPlaying() {
    paused = false;
    needsPlayButton = false;
    showSpinner = false;
    if (spinnerTimer !== null) {
      clearTimeout(spinnerTimer);
      spinnerTimer = null;
    }
    hasPlayed = true;
    syncAudio();
    scheduleFilmstrip();
  }

  // ---- Filmstrip ----

  /** Timeline zoom last reported; the first run uses it if the user zoomed before the start delay. */
  let filmZoom = 1;
  let filmViewWidth = 0;
  const FILM_ZOOM_MAX_FRAMES = 60;

  /** Starts the filmstrip after a short delay so it never competes with the start of playback. */
  function scheduleFilmstrip() {
    if (filmStarted || filmTimer !== null) return;
    filmTimer = setTimeout(() => {
      filmTimer = null;
      startFilmstrip();
    }, FILM_START_DELAY_MS);
  }

  function startFilmstrip() {
    if (filmStarted || !mainEl) return;
    filmStarted = true;
    runFilmstrip();
  }

  /** (Re)generates the strip for the current zoom, aborting any previous run. */
  function runFilmstrip() {
    if (!mainEl) return;
    filmAbort?.abort();
    const view = filmViewWidth > 0 ? filmViewWidth : Math.max(1, mainEl.clientWidth - 32);
    const cap = filmZoom > 1 ? FILM_ZOOM_MAX_FRAMES : FILM_MAX_FRAMES;
    const count = Math.min(cap, filmstripCount(view * filmZoom, aspect, FILM_HEIGHT));
    frames = new Array<string>(count).fill('');
    filmAbort = new AbortController();
    const signal = filmAbort.signal;
    void (async () => {
      try {
        for await (const f of makeFilmstrip(videoSrc, count, FILM_HEIGHT, signal)) {
          if (signal.aborted) return;
          frames[f.index] = f.dataUrl;
        }
      } catch {
        // The strip is decoration; ignore failures.
      }
    })();
  }

  /** Timeline zoom changed: redraw the strip over the wider track (only once the first run started). */
  function onTimelineZoom(zoom: number, viewWidth: number) {
    filmZoom = zoom;
    filmViewWidth = viewWidth;
    if (filmStarted) runFilmstrip();
  }

  // ---- Details panel handlers ----

  async function onRename(stem: string) {
    await renameRecording(id, stem);
    applyPatch({ fileName: stem + splitExtension(displayName) });
  }

  async function onFavorite(v: boolean) {
    await setFavorite(id, v);
    applyPatch({ isFavorite: v });
  }

  function onReveal() {
    void revealRecording(id);
  }

  // ---- Save edits ----

  const mode = $derived<TrimMode>(exactCut ? 'precise' : 'lossless');

  function resetTrim() {
    selection = clampSelection({ startMs: 0, endMs: durationMs }, durationMs);
    trimError = '';
  }

  function openSavePopover() {
    if (!trimmed || trimRunning) return;
    savePopover = !savePopover;
  }

  // Debounced lossless snap readout while the popover is open.
  $effect(() => {
    const open = savePopover;
    const m = mode;
    const sel = selection;
    if (previewTimer !== null) {
      clearTimeout(previewTimer);
      previewTimer = null;
    }
    if (!open || m !== 'lossless') {
      snapText = '';
      return;
    }
    const seq = ++previewSeq;
    previewTimer = setTimeout(() => {
      previewTimer = null;
      getTrimPreview({ id, startMs: sel.startMs, endMs: sel.endMs, mode: m }).then(
        (p) => {
          if (seq === previewSeq && !cancelled) {
            snapText = snapReadout(sel, { startMs: p.actualStartMs, endMs: p.actualEndMs });
          }
        },
        () => {
          if (seq === previewSeq) snapText = '';
        },
      );
    }, PREVIEW_DEBOUNCE_MS);
    return () => {
      if (previewTimer !== null) {
        clearTimeout(previewTimer);
        previewTimer = null;
      }
    };
  });

  // Close the popover on a pointer down outside it.
  $effect(() => {
    if (!savePopover) return;
    const onDown = (e: PointerEvent) => {
      if (saveWrap && e.target instanceof Node && !saveWrap.contains(e.target)) {
        savePopover = false;
      }
    };
    document.addEventListener('pointerdown', onDown);
    return () => document.removeEventListener('pointerdown', onDown);
  });

  async function confirmSave() {
    if (trimRunning) return;
    trimError = '';
    savedNewId = null;
    trimPercent = 0;
    trimJobId = null;
    trimRunning = true;
    savePopover = false;
    try {
      const started = await startTrim({
        id,
        startMs: selection.startMs,
        endMs: selection.endMs,
        mode,
        overwrite: false,
        keepTimestamp: replaceOriginal,
      });
      trimJobId = started.jobId;
    } catch (err) {
      trimRunning = false;
      trimError = errorText(err);
    }
  }

  function cancelSave() {
    if (trimJobId !== null) void cancelTrim(trimJobId).catch(() => {});
  }

  async function finishTrim(newId: string) {
    if (replaceOriginal) {
      try {
        await deleteRecording(id);
      } catch (err) {
        // Stay on screen so the message is seen; the user can still open the new clip.
        savedNewId = newId;
        trimError = `Saved the new clip but could not remove the original: ${errorText(err)}`;
        return;
      }
    }
    openRecording(newId, undefined);
  }

  // ---- Lifecycle ----

  /**
   * Resolves the media base first, then mounts the <video> with its source (so the element never
   * starts on the slow fallback) and asks for playback.
   */
  async function startVideo() {
    const base = await getMediaBase();
    if (cancelled) return;
    mediaBaseUrl = base;
    videoSrc = mediaUrl(base, 'rec', id);
    await tick();
    if (cancelled) return;
    // Autoplay can be refused by the webview: offer a big play button instead.
    video?.play().catch(() => {
      if (!cancelled && !hasPlayed) needsPlayButton = true;
    });
  }

  onMount(() => {
    // Keyboard handling only applies while the player has focus; never steal it from a text field.
    if (root && !isTextTarget(document.activeElement)) root.focus();

    void startVideo();
    spinnerTimer = setTimeout(() => {
      spinnerTimer = null;
      if (!hasPlayed && !cancelled) showSpinner = true;
    }, SPINNER_DELAY_MS);

    openPlayer(id).then(
      (opened) => {
        if (cancelled) return;
        dto = opened;
        if (duration <= 0) duration = opened.durationMs / 1000;
        if (opened.playable) {
          void getMediaBase().then((base) => {
            if (cancelled) return;
            mediaBaseUrl = base;
            loadAudioInBackground(opened);
          });
        }
      },
      (err: unknown) => {
        if (!cancelled) audioNotice = errorText(err);
      },
    );

    getRecordingDetails(id).then(
      (d) => {
        if (!cancelled) path = d.path;
      },
      () => {},
    );

    const gameId = untrack(() => $playerRow?.gameId ?? null);
    if (gameId) {
      getGameIcons([gameId]).then((m) => {
        if (!cancelled) gameIcon = m[gameId];
      });
    }

    onTrimProgress((p) => {
      if (trimRunning && (trimJobId === null || p.jobId === trimJobId)) trimPercent = p.percent;
    }).then((un) => (cancelled ? un() : unlisteners.push(un)));
    onTrimDone((d) => {
      if (!trimRunning || (trimJobId !== null && d.jobId !== trimJobId)) return;
      trimRunning = false;
      trimJobId = null;
      if (d.ok && d.newId) {
        void finishTrim(d.newId);
      } else if (!d.ok) {
        trimError = d.error ?? 'Trim failed.';
      }
    }).then((un) => (cancelled ? un() : unlisteners.push(un)));
  });

  onDestroy(() => {
    cancelled = true;
    if (clickTimer !== null) clearTimeout(clickTimer);
    if (spinnerTimer !== null) clearTimeout(spinnerTimer);
    if (seekFrame) cancelAnimationFrame(seekFrame);
    if (previewTimer !== null) clearTimeout(previewTimer);
    if (filmTimer !== null) clearTimeout(filmTimer);
    filmAbort?.abort();
    for (const un of unlisteners) un();
    unlisteners = [];
    teardownAudio();
    mixer.bindVideo(null);
    if (isFullscreen) void document.exitFullscreen().catch(() => {});
  });

  // Routes the video element's audio to the chosen output while the player is open.
  $effect(() => {
    const el = video;
    if (!el) return;
    mixer.bindVideo(el);
    return () => mixer.bindVideo(null);
  });

  // Remember the volume for the next clip and the next app session.
  $effect(() => {
    saveVolume({ volume: volState.volume, last: volState.last });
  });

  // Keep the video (or the muted video plus mixer) in line with volume, mute and track state.
  $effect(() => {
    const v = volume;
    const m = muted;
    const t = tracks;
    const active = mixedActive;
    if (!video) return;
    if (active) {
      video.muted = true;
      mixer.apply(t, v, m);
    } else {
      video.volume = clampVolume(v);
      video.muted = m;
    }
  });

  // Selection follows the real duration while it still covers the whole clip.
  $effect(() => {
    const d = durationMs;
    untrack(() => {
      if (d <= 0) return;
      const full =
        selection.endMs <= 0 || (selection.startMs <= 0 && selection.endMs === lastFullMs);
      if (full) selection = clampSelection({ startMs: 0, endMs: d }, d);
      lastFullMs = d;
    });
  });

  $effect(() => {
    const onChange = () => {
      isFullscreen = document.fullscreenElement !== null;
    };
    document.addEventListener('fullscreenchange', onChange);
    return () => document.removeEventListener('fullscreenchange', onChange);
  });
</script>

<!-- Keyboard shortcuts are scoped to this root; the region is a custom media widget. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="player"
  bind:this={root}
  tabindex="-1"
  role="application"
  aria-label="Video player"
  onkeydown={onKeydown}
>
  <div class="main" bind:this={mainEl}>
    <header class="topbar">
      <button type="button" class="icon-btn" aria-label="Back" onclick={onclose}>
        <Icon name="back" />
      </button>
      {#if displayName !== ''}
        <span class="title" title={displayName}>{displayName}</span>
      {/if}
      <span class="spacer"></span>
      <div class="topbar-menu">
        <RecordingMenu
          bind:this={menuRef}
          id={id}
          fileName={displayName}
          ready={true}
          isFavorite={row.isFavorite}
          onchanged={onRecordingChanged}
        />
      </div>
    </header>

    {#if audioNotice !== null}
      <p class="notice note-row" role="status">
        <span>Multi-track audio unavailable: {audioNotice}. Playing the default track.</span>
        <button
          type="button"
          class="icon-btn small"
          aria-label="Dismiss note"
          onclick={() => (audioNotice = null)}><Icon name="close" size={16} /></button
        >
      </p>
    {/if}

    <div class="stage" style:background-image={`url("${thumbUrl(id)}")`}>
      {#if dto && !dto.playable}
        <div class="notice error" role="alert">
          MKV files can't be played in the app yet. Open the file in your default player.
        </div>
      {:else if videoError}
        <div class="notice error" role="alert">
          This video can't be played (unsupported codec or missing file).
        </div>
      {:else if videoSrc !== ''}
        <!-- svelte-ignore a11y_media_has_caption -->
        <video
          class="screen"
          bind:this={video}
          src={videoSrc}
          crossorigin="anonymous"
          poster={thumbUrl(id)}
          autoplay
          preload="auto"
          playsinline
          onclick={onVideoClick}
          ondblclick={onVideoDblClick}
          oncontextmenu={onVideoContextMenu}
          onplay={() => {
            paused = false;
            needsPlayButton = false;
            mixer.resume().catch(() => {});
            startSyncTimer();
            syncAudio();
          }}
          onplaying={onPlaying}
          onpause={() => {
            paused = true;
            stopSyncTimer();
            syncAudio();
          }}
          onseeking={syncAudio}
          onseeked={syncAudio}
          onwaiting={syncAudio}
          onratechange={() => {
            if (video) rate = video.playbackRate;
            syncAudio();
          }}
          ontimeupdate={onTimeUpdate}
          ondurationchange={() => {
            if (video && Number.isFinite(video.duration) && video.duration > 0) {
              duration = video.duration;
            }
          }}
          onerror={() => {
            videoError = true;
          }}
        ></video>
        {#if needsPlayButton}
          <button type="button" class="big-play" aria-label="Play" onclick={togglePlay}>
            <Icon name="play" size={40} />
          </button>
        {/if}
        {#if showSpinner}
          <div class="spinner" role="status" aria-label="Buffering"></div>
        {/if}
      {/if}
    </div>

    <div class="under">
      <Controls
        {paused}
        {now}
        {duration}
        {volume}
        {muted}
        {rate}
        fullscreen={isFullscreen}
        ontoggle={togglePlay}
        onvolume={(v) => (volState = setLevel(volState, v))}
        onmute={() => (volState = toggleMute(volState))}
        onrate={setRate}
        onfullscreen={() => void toggleFullscreen()}
      >
        {#snippet leading()}
          {#if showAudioMenu}
            <AudioMenu infos={trackInfos} {tracks} {mixer} ontracks={(next) => (tracks = next)} />
          {/if}
        {/snippet}
      </Controls>

      <Timeline
        {durationMs}
        nowMs={now * 1000}
        {selection}
        {frames}
        onseek={seekMs}
        onseekfree={seekFree}
        onscrub={onScrub}
        onselection={(sel) => (selection = sel)}
        onzoom={onTimelineZoom}
      />

      <div class="actions">
        <span class="spacer"></span>

        {#if trimRunning}
          <div class="progress" role="status">
            <div
              class="bar"
              role="progressbar"
              aria-label="Saving edits"
              aria-valuemin="0"
              aria-valuemax="100"
              aria-valuenow={Math.round(trimPercent)}
            >
              <span style:width={`${Math.max(0, Math.min(100, trimPercent))}%`}></span>
            </div>
            <button type="button" class="btn" onclick={cancelSave}>Cancel</button>
          </div>
        {:else}
          {#if trimmed}
            <button type="button" class="btn" onclick={resetTrim}>
              <Icon name="reset" size={16} /> Reset
            </button>
          {/if}
          <div class="save-wrap" bind:this={saveWrap}>
            <button
              type="button"
              class="btn primary"
              disabled={!trimmed}
              aria-haspopup="dialog"
              aria-expanded={savePopover}
              onclick={openSavePopover}>Save Edits</button
            >
            {#if savePopover}
              <div class="popover" role="dialog" aria-label="Save edits">
                <fieldset>
                  <legend>Save as</legend>
                  <label
                    ><input type="radio" name="save-kind" checked={!replaceOriginal} onchange={() => (replaceOriginal = false)} />
                    Save as copy</label
                  >
                  <label
                    ><input type="radio" name="save-kind" checked={replaceOriginal} onchange={() => (replaceOriginal = true)} />
                    Save clip</label
                  >
                </fieldset>
                {#if replaceOriginal}
                  <p class="dim small">Shortens the original. The uncut version moves to the Recycle Bin.</p>
                {/if}
                <label class="check"
                  ><input type="checkbox" bind:checked={exactCut} /> Exact cut (slower, re-encodes)</label
                >
                {#if !exactCut && snapText !== ''}
                  <p class="dim small">{snapText}</p>
                {/if}
                <div class="popover-actions">
                  <button type="button" class="btn" onclick={() => (savePopover = false)}
                    >Cancel</button
                  >
                  <button type="button" class="btn primary" onclick={confirmSave}>Confirm</button>
                </div>
              </div>
            {/if}
          </div>
        {/if}
      </div>

      {#if trimError !== ''}
        <p class="notice error" role="alert">
          {trimError}
          {#if savedNewId !== null}
            <button
              type="button"
              class="btn"
              onclick={() => savedNewId !== null && openRecording(savedNewId, undefined)}
              >Open new clip</button
            >
          {/if}
        </p>
      {/if}
    </div>
  </div>

  <aside class="side" aria-label="Recording details">
    <DetailsPanel
      {row}
      {path}
      {gameIcon}
      onrename={onRename}
      onfavorite={onFavorite}
      onreveal={onReveal}
    />
  </aside>
</div>

<style>
  .player {
    display: flex;
    flex-direction: row;
    height: 100%;
    min-height: 0;
    background: var(--bg);
    outline: none;
  }

  .main {
    flex: 1 1 0;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg);
  }

  .side {
    flex: 0 0 300px;
    width: 300px;
    min-height: 0;
    overflow-y: auto;
    background: var(--surface);
    border-left: 1px solid var(--border);
  }

  .topbar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    background: var(--surface);
    border-bottom: 1px solid var(--border);
  }

  .title {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: 600;
  }

  .spacer {
    flex: 1 1 auto;
  }

  .topbar-menu {
    display: inline-flex;
  }

  .icon-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 6px;
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius);
    color: inherit;
    cursor: pointer;
  }

  .icon-btn:hover {
    border-color: var(--border);
    background: var(--surface-2);
  }

  .icon-btn.small {
    padding: 2px;
  }

  .btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 7px 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: inherit;
    cursor: pointer;
    white-space: nowrap;
  }

  .btn:hover:not(:disabled) {
    border-color: var(--accent);
  }

  .btn.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  .btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .icon-btn:focus-visible,
  .btn:focus-visible,
  .big-play:focus-visible,
  input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .stage {
    flex: 1 1 auto;
    min-height: 0;
    position: relative;
    display: flex;
    background-color: #000;
    background-size: contain;
    background-position: center;
    background-repeat: no-repeat;
  }

  .screen {
    display: block;
    width: 100%;
    height: 100%;
    object-fit: contain;
    background: #000;
    cursor: pointer;
  }

  .big-play {
    position: absolute;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 76px;
    height: 76px;
    border-radius: 50%;
    border: none;
    background: rgba(0, 0, 0, 0.6);
    color: #fff;
    cursor: pointer;
  }

  .big-play:hover {
    background: var(--accent);
  }

  .spinner {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 3px;
    overflow: hidden;
    pointer-events: none;
    background: rgba(255, 255, 255, 0.15);
  }

  .spinner::after {
    content: '';
    position: absolute;
    top: 0;
    bottom: 0;
    width: 30%;
    background: var(--accent);
    animation: slide 1s linear infinite;
  }

  @keyframes slide {
    from {
      left: -30%;
    }
    to {
      left: 100%;
    }
  }

  .stage .notice {
    margin: 16px;
    align-self: flex-start;
  }

  .under {
    flex: 0 0 auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 8px 16px 12px;
    background: var(--surface);
    border-top: 1px solid var(--border);
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .save-wrap {
    position: relative;
  }

  .popover {
    position: absolute;
    bottom: calc(100% + 8px);
    right: 0;
    z-index: 3;
    width: min(360px, 90vw);
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 6px 24px rgba(0, 0, 0, 0.35);
  }

  .popover fieldset {
    margin: 0;
    padding: 0;
    border: none;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .popover legend {
    padding: 0;
    margin-bottom: 4px;
    font-weight: 600;
  }

  .popover label {
    display: flex;
    align-items: center;
    gap: 8px;
    cursor: pointer;
  }

  .popover-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .progress {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .bar {
    width: 160px;
    height: 8px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: 4px;
    overflow: hidden;
  }

  .bar span {
    display: block;
    height: 100%;
    background: var(--accent);
  }

  .notice {
    margin: 0;
    padding: 8px 12px;
    border-radius: var(--radius);
    background: var(--surface-2);
  }

  .note-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    margin: 8px 16px 0;
    font-size: 13px;
  }

  .error {
    color: var(--danger);
    border: 1px solid var(--danger);
  }

  .dim {
    color: var(--text-dim);
  }

  .small {
    margin: 0;
    font-size: 12px;
  }

  @media (max-width: 760px) {
    .player {
      flex-direction: column;
      overflow-y: auto;
    }

    .main {
      flex: 0 0 auto;
      height: 80vh;
    }

    .side {
      flex: 0 0 auto;
      width: 100%;
      border-left: none;
      border-top: 1px solid var(--border);
      overflow-y: visible;
    }
  }
</style>
