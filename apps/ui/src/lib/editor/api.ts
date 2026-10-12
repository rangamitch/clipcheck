/**
 * Typed wrappers around the video editor's Tauri commands and events.
 *
 * Command names are the Rust function names (`editor_*`). Tauri maps the camelCase argument
 * names used here to the snake_case Rust parameters. Errors are rejected as strings and are not
 * caught here; the store turns them into toasts.
 */
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  ApplyReply,
  Canvas,
  Command,
  EditorSettings,
  EditorSnapshot,
  EstimateReply,
  ExportDefaults,
  ExportSettings,
  JobDto,
  LoudnessReport,
  MediaRef,
  MediaStatus,
  Preset,
  ProjectSummary,
  RenderPlan,
  SnapPointDto,
  Tick,
  UiState,
} from './types';

/** Payload of the `peaks-ready` event. */
export interface PeaksReady {
  mediaId: string;
  stream: number;
}

export const editorApi = {
  /** Lists saved projects, newest first. */
  listProjects(): Promise<ProjectSummary[]> {
    return invoke<ProjectSummary[]>('editor_list_projects');
  },

  /** Creates a project and opens it. `canvas` null uses the default canvas rule. */
  newProject(name: string, canvas: Canvas | null, recordingIds: string[]): Promise<EditorSnapshot> {
    return invoke<EditorSnapshot>('editor_new_project', { name, canvas, recordingIds });
  },

  /** Opens a saved project as the active session. */
  openProject(id: string): Promise<EditorSnapshot> {
    return invoke<EditorSnapshot>('editor_open_project', { id });
  },

  /** Opens a project's autosave instead of the saved file (crash recovery). */
  recoverAutosave(id: string): Promise<EditorSnapshot> {
    return invoke<EditorSnapshot>('editor_recover_autosave', { id });
  },

  /** Saves the active session to its project file. */
  saveProject(): Promise<void> {
    return invoke<void>('editor_save_project');
  },

  /** Writes the active session to its autosave file (no-op when clean). */
  autosave(): Promise<void> {
    return invoke<void>('editor_autosave');
  },

  /** Closes the active session. `discardAutosave` deletes the autosave file. */
  closeProject(discardAutosave: boolean): Promise<void> {
    return invoke<void>('editor_close_project', { discardAutosave });
  },

  /** Renames a saved project. */
  renameProject(id: string, name: string): Promise<void> {
    return invoke<void>('editor_rename_project', { id, name });
  },

  /** Duplicates a saved project and returns the new id. */
  duplicateProject(id: string): Promise<string> {
    return invoke<string>('editor_duplicate_project', { id });
  },

  /** Deletes a saved project (moved to the Recycle Bin). */
  deleteProject(id: string): Promise<void> {
    return invoke<void>('editor_delete_project', { id });
  },

  /** Applies one edit command to the open session. */
  apply(command: Command): Promise<ApplyReply> {
    return invoke<ApplyReply>('editor_apply', { command });
  },

  /** Undoes the last edit. */
  undo(): Promise<EditorSnapshot> {
    return invoke<EditorSnapshot>('editor_undo');
  },

  /** Redoes the last undone edit. */
  redo(): Promise<EditorSnapshot> {
    return invoke<EditorSnapshot>('editor_redo');
  },

  /** Stores cosmetic UI state (zoom, scroll, playhead) without touching undo or dirty flags. */
  setUiState(ui: UiState): Promise<void> {
    return invoke<void>('editor_set_ui_state', { ui });
  },

  /** Render plan for a range; null bounds mean the whole timeline. */
  plan(start: Tick | null, end: Tick | null): Promise<RenderPlan> {
    return invoke<RenderPlan>('editor_plan', { start, end });
  },

  /** Snap points for a drag, leaving out the clips in `exclude`. */
  snapPoints(exclude: string[], playhead: Tick): Promise<SnapPointDto[]> {
    return invoke<SnapPointDto[]>('editor_snap_points', { exclude, playhead });
  },

  /** Number of clips on the session clipboard. */
  clipboardLen(): Promise<number> {
    return invoke<number>('editor_clipboard_len');
  },

  /** Probes a file on disk and returns a new media entry. */
  probeMedia(path: string): Promise<MediaRef> {
    return invoke<MediaRef>('editor_probe_media', { path });
  },

  /** Resolves a library recording and probes it. */
  mediaFromRecording(recordingId: string): Promise<MediaRef> {
    return invoke<MediaRef>('editor_media_from_recording', { recordingId });
  },

  /** Missing or changed flags for every media of the open project. */
  mediaStatus(): Promise<MediaStatus[]> {
    return invoke<MediaStatus[]>('editor_media_status');
  },

  /** Peak waveform values (0..=1) for one audio stream of a media file. */
  waveform(mediaId: string, stream: number, buckets: number): Promise<number[]> {
    return invoke<number[]>('editor_waveform', { mediaId, stream, buckets });
  },

  /** JPEG thumbnails as data URLs for the given source times (ms). */
  thumbnails(mediaId: string, timesMs: number[], height: number): Promise<string[]> {
    return invoke<string[]>('editor_thumbnails', { mediaId, timesMs, height });
  },

  /** A playable URL for a media file of the open project. */
  mediaUrl(mediaId: string): Promise<string> {
    return invoke<string>('editor_media_url', { mediaId });
  },

  /** Measures the loudness of an audio clip's used source range. */
  measureClipLoudness(clipId: string): Promise<LoudnessReport> {
    return invoke<LoudnessReport>('editor_measure_clip_loudness', { clipId });
  },

  /** Export dialog defaults for the open project. */
  exportDefaults(): Promise<ExportDefaults> {
    return invoke<ExportDefaults>('editor_export_defaults');
  },

  /** Size and duration estimate, with validation problems. */
  estimateExport(settings: ExportSettings): Promise<EstimateReply> {
    return invoke<EstimateReply>('editor_estimate_export', { settings });
  },

  /** Smart-lossless eligibility (backend-defined JSON). */
  checkSmartExport(settings: ExportSettings): Promise<unknown> {
    return invoke<unknown>('editor_check_smart_export', { settings });
  },

  /** Starts or queues an export of the open project. */
  exportStart(settings: ExportSettings): Promise<JobDto> {
    return invoke<JobDto>('editor_export_start', { settings });
  },

  /** Cancels a running or queued export job. */
  exportCancel(jobId: string): Promise<void> {
    return invoke<void>('editor_export_cancel', { jobId });
  },

  /** Pauses or resumes an export job. */
  exportPause(jobId: string, paused: boolean): Promise<void> {
    return invoke<void>('editor_export_pause', { jobId, paused });
  },

  /** Lets a job run despite the pause-while-recording rule. */
  exportPauseOverride(jobId: string): Promise<void> {
    return invoke<void>('editor_export_pause_override', { jobId });
  },

  /** Current export queue. */
  exportQueue(): Promise<JobDto[]> {
    return invoke<JobDto[]>('editor_export_queue');
  },

  /** Removes a finished, failed or cancelled job from the list. */
  exportDismiss(jobId: string): Promise<void> {
    return invoke<void>('editor_export_dismiss', { jobId });
  },

  /** Moves a queued job to `index`. */
  exportReorder(jobId: string, index: number): Promise<void> {
    return invoke<void>('editor_export_reorder', { jobId, index });
  },

  /** Opens a finished export in the default player (backend checks the path is in the export folder). */
  openExport(path: string): Promise<void> {
    return invoke<void>('editor_open_export', { path });
  },

  /**
   * Exports one still frame at `at` right away and returns its file path. `dir` and
   * `nameTemplate` are optional; null uses the default folder and file name.
   */
  exportStill(
    at: Tick,
    format: string,
    quality: number,
    dir?: string | null,
    nameTemplate?: string | null,
  ): Promise<string> {
    return invoke<string>('editor_export_still', {
      at,
      format,
      quality,
      dir: dir ?? null,
      nameTemplate: nameTemplate ?? null,
    });
  },

  /** The current project snapshot of the open session, or null when none is open. */
  currentSnapshot(): Promise<EditorSnapshot | null> {
    return invoke<EditorSnapshot | null>('editor_current_snapshot');
  },

  /** Saves a user preset and returns all user presets. */
  savePreset(preset: Preset): Promise<Preset[]> {
    return invoke<Preset[]>('editor_save_preset', { preset });
  },

  /** Deletes a user preset and returns the remaining user presets. */
  deletePreset(id: string): Promise<Preset[]> {
    return invoke<Preset[]>('editor_delete_preset', { id });
  },

  /** Editor and export settings. */
  getSettings(): Promise<EditorSettings> {
    return invoke<EditorSettings>('editor_get_settings');
  },

  /** Saves editor settings and returns the normalised result. */
  setSettings(settings: EditorSettings): Promise<EditorSettings> {
    return invoke<EditorSettings>('editor_set_settings', { settings });
  },
};

/** Subscribes to `export-progress`. Resolves with the unlisten function. */
export function onExportProgress(cb: (job: JobDto) => void): Promise<UnlistenFn> {
  return listen<JobDto>('export-progress', (event) => cb(event.payload));
}

/** Subscribes to `export-done`. Resolves with the unlisten function. */
export function onExportDone(cb: (job: JobDto) => void): Promise<UnlistenFn> {
  return listen<JobDto>('export-done', (event) => cb(event.payload));
}

/** Subscribes to `export-failed`. Resolves with the unlisten function. */
export function onExportFailed(cb: (job: JobDto) => void): Promise<UnlistenFn> {
  return listen<JobDto>('export-failed', (event) => cb(event.payload));
}

/** Subscribes to `export-queue`. Resolves with the unlisten function. */
export function onExportQueue(cb: (jobs: JobDto[]) => void): Promise<UnlistenFn> {
  return listen<JobDto[]>('export-queue', (event) => cb(event.payload));
}

/** Subscribes to `peaks-ready`. Resolves with the unlisten function. */
export function onPeaksReady(cb: (p: PeaksReady) => void): Promise<UnlistenFn> {
  return listen<PeaksReady>('peaks-ready', (event) => cb(event.payload));
}
