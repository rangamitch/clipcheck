import { beforeEach, describe, expect, it, vi } from 'vitest';

const invokeMock = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

const listenMock = vi.fn();
vi.mock('@tauri-apps/api/event', () => ({
  listen: (...args: unknown[]) => listenMock(...args),
}));

import {
  editorApi,
  onExportDone,
  onExportFailed,
  onExportProgress,
  onExportQueue,
  onPeaksReady,
} from './api';
import type { Canvas, Command, EditorSettings, ExportSettings, Preset } from './types';
import { defaultCanvas, defaultExportSettings } from './types';

const canvas: Canvas = defaultCanvas();
const settings: ExportSettings = defaultExportSettings();
const command: Command = { type: 'split_at', time: 1000, ids: ['c1'] };
const preset: Preset = { id: 'p', name: 'Preset', settings } as unknown as Preset;
const editorSettings = {} as EditorSettings;
const uiState = { zoom_px_per_sec: 80, scroll_x: 0, playhead: 0 };

/** One row per `editorApi` method: name, call, Tauri command, expected argument object (or none). */
type Row = [name: string, run: () => Promise<unknown>, cmd: string, args?: Record<string, unknown>];

const rows: Row[] = [
  ['listEncoders', () => editorApi.listEncoders('h264'), 'editor_list_encoders', { codec: 'h264' }],
  ['listProjects', () => editorApi.listProjects(), 'editor_list_projects'],
  ['newProject', () => editorApi.newProject('Demo', null, ['r1']), 'editor_new_project', { name: 'Demo', canvas: null, recordingIds: ['r1'] }],
  ['newProject', () => editorApi.newProject('Demo', canvas, []), 'editor_new_project', { name: 'Demo', canvas, recordingIds: [] }],
  ['openProject', () => editorApi.openProject('proj-1'), 'editor_open_project', { id: 'proj-1' }],
  ['recoverAutosave', () => editorApi.recoverAutosave('proj-1'), 'editor_recover_autosave', { id: 'proj-1' }],
  ['saveProject', () => editorApi.saveProject(), 'editor_save_project'],
  ['autosave', () => editorApi.autosave(), 'editor_autosave'],
  ['closeProject', () => editorApi.closeProject(true), 'editor_close_project', { discardAutosave: true }],
  ['closeProject', () => editorApi.closeProject(false), 'editor_close_project', { discardAutosave: false }],
  ['renameProject', () => editorApi.renameProject('proj-1', 'New'), 'editor_rename_project', { id: 'proj-1', name: 'New' }],
  ['duplicateProject', () => editorApi.duplicateProject('proj-1'), 'editor_duplicate_project', { id: 'proj-1' }],
  ['deleteProject', () => editorApi.deleteProject('proj-1'), 'editor_delete_project', { id: 'proj-1' }],
  ['apply', () => editorApi.apply(command), 'editor_apply', { command }],
  ['undo', () => editorApi.undo(), 'editor_undo'],
  ['redo', () => editorApi.redo(), 'editor_redo'],
  ['setUiState', () => editorApi.setUiState(uiState), 'editor_set_ui_state', { ui: uiState }],
  ['plan', () => editorApi.plan(0, null), 'editor_plan', { start: 0, end: null }],
  ['plan', () => editorApi.plan(100, 900), 'editor_plan', { start: 100, end: 900 }],
  ['snapPoints', () => editorApi.snapPoints(['c1'], 500), 'editor_snap_points', { exclude: ['c1'], playhead: 500 }],
  ['clipboardLen', () => editorApi.clipboardLen(), 'editor_clipboard_len'],
  ['probeMedia', () => editorApi.probeMedia('D:/clips/a.mp4'), 'editor_probe_media', { path: 'D:/clips/a.mp4' }],
  ['mediaFromRecording', () => editorApi.mediaFromRecording('rec-9'), 'editor_media_from_recording', { recordingId: 'rec-9' }],
  ['mediaStatus', () => editorApi.mediaStatus(), 'editor_media_status'],
  ['waveform', () => editorApi.waveform('m1', 0, 200), 'editor_waveform', { mediaId: 'm1', stream: 0, buckets: 200 }],
  ['thumbnails', () => editorApi.thumbnails('m1', [0, 1000], 90), 'editor_thumbnails', { mediaId: 'm1', timesMs: [0, 1000], height: 90 }],
  ['mediaUrl', () => editorApi.mediaUrl('m1'), 'editor_media_url', { mediaId: 'm1' }],
  ['audioStreamUrl', () => editorApi.audioStreamUrl('m1', 1), 'editor_audio_stream_url', { mediaId: 'm1', stream: 1 }],
  ['measureClipLoudness', () => editorApi.measureClipLoudness('c1'), 'editor_measure_clip_loudness', { clipId: 'c1' }],
  ['exportDefaults', () => editorApi.exportDefaults(), 'editor_export_defaults'],
  ['estimateExport', () => editorApi.estimateExport(settings), 'editor_estimate_export', { settings }],
  ['checkSmartExport', () => editorApi.checkSmartExport(settings), 'editor_check_smart_export', { settings }],
  ['exportStart', () => editorApi.exportStart(settings), 'editor_export_start', { settings }],
  ['exportCancel', () => editorApi.exportCancel('job-1'), 'editor_export_cancel', { jobId: 'job-1' }],
  ['exportPause', () => editorApi.exportPause('job-1', true), 'editor_export_pause', { jobId: 'job-1', paused: true }],
  ['exportPause', () => editorApi.exportPause('job-1', false), 'editor_export_pause', { jobId: 'job-1', paused: false }],
  ['exportPauseOverride', () => editorApi.exportPauseOverride('job-1'), 'editor_export_pause_override', { jobId: 'job-1' }],
  ['exportQueue', () => editorApi.exportQueue(), 'editor_export_queue'],
  ['exportDismiss', () => editorApi.exportDismiss('job-1'), 'editor_export_dismiss', { jobId: 'job-1' }],
  ['exportReorder', () => editorApi.exportReorder('job-1', 2), 'editor_export_reorder', { jobId: 'job-1', index: 2 }],
  ['openExport', () => editorApi.openExport('D:/exports/a.mp4'), 'editor_open_export', { path: 'D:/exports/a.mp4' }],
  ['exportStill', () => editorApi.exportStill(5000, 'png', 90), 'editor_export_still', { at: 5000, format: 'png', quality: 90, dir: null, nameTemplate: null }],
  ['exportStill', () => editorApi.exportStill(5000, 'jpg', 80, 'D:/stills', '{project}_{time}'), 'editor_export_still', { at: 5000, format: 'jpg', quality: 80, dir: 'D:/stills', nameTemplate: '{project}_{time}' }],
  ['currentSnapshot', () => editorApi.currentSnapshot(), 'editor_current_snapshot'],
  ['savePreset', () => editorApi.savePreset(preset), 'editor_save_preset', { preset }],
  ['deletePreset', () => editorApi.deletePreset('p'), 'editor_delete_preset', { id: 'p' }],
  ['getSettings', () => editorApi.getSettings(), 'editor_get_settings'],
  ['setSettings', () => editorApi.setSettings(editorSettings), 'editor_set_settings', { settings: editorSettings }],
  ['listPresets', () => editorApi.listPresets(), 'editor_list_presets'],
  ['cacheInfo', () => editorApi.cacheInfo(), 'editor_cache_info'],
  ['clearCache', () => editorApi.clearCache(), 'editor_clear_cache'],
];

beforeEach(() => {
  invokeMock.mockReset();
  listenMock.mockReset();
  invokeMock.mockResolvedValue(undefined);
});

describe('editorApi command names and arguments', () => {
  it.each(rows)('%s calls the Tauri command with the expected arguments (%#)', async (_name, run, cmd, args) => {
    await run();
    expect(invokeMock).toHaveBeenCalledTimes(1);
    if (args === undefined) {
      expect(invokeMock).toHaveBeenCalledWith(cmd);
    } else {
      expect(invokeMock).toHaveBeenCalledWith(cmd, args);
    }
  });

  it.each(rows)('%s sends exactly one invoke call', async (_name, run) => {
    await run();
    expect(invokeMock.mock.calls.length).toBe(1);
  });

  it('every command name uses the editor_ prefix', () => {
    for (const [, , cmd] of rows) expect(cmd.startsWith('editor_')).toBe(true);
  });

  it('every command name is unique per method (no two methods share a command)', () => {
    const byCmd = new Map<string, string>();
    for (const [name, , cmd] of rows) {
      const prev = byCmd.get(cmd);
      if (prev !== undefined) expect(prev).toBe(name);
      byCmd.set(cmd, name);
    }
  });

  it('argument keys are camelCase (no snake_case keys sent to the backend)', () => {
    for (const [, , , args] of rows) {
      if (args === undefined) continue;
      for (const key of Object.keys(args)) {
        expect(key).not.toMatch(/_/);
      }
    }
  });

  it('the table covers every method of editorApi', () => {
    const covered = new Set(rows.map((r) => r[0]));
    const methods = Object.keys(editorApi).sort();
    expect(methods.length).toBe(46);
    expect(methods.every((m) => covered.has(m))).toBe(true);
  });
});

describe('editorApi results and errors', () => {
  it('returns the value the Tauri command resolves with', async () => {
    invokeMock.mockResolvedValueOnce(['p1', 'p2']);
    await expect(editorApi.listProjects()).resolves.toEqual(['p1', 'p2']);
  });

  it('duplicateProject returns the new id string', async () => {
    invokeMock.mockResolvedValueOnce('new-id');
    await expect(editorApi.duplicateProject('x')).resolves.toBe('new-id');
  });

  it('clipboardLen returns the number from the backend', async () => {
    invokeMock.mockResolvedValueOnce(3);
    await expect(editorApi.clipboardLen()).resolves.toBe(3);
  });

  it('a backend error string is rejected unchanged', async () => {
    invokeMock.mockRejectedValueOnce('no project is open');
    await expect(editorApi.saveProject()).rejects.toBe('no project is open');
  });

  it('a rejected undo propagates its error', async () => {
    invokeMock.mockRejectedValueOnce('nothing to undo');
    await expect(editorApi.undo()).rejects.toBe('nothing to undo');
  });

  it('an error from apply propagates without being wrapped', async () => {
    invokeMock.mockRejectedValueOnce('overlap');
    await expect(editorApi.apply(command)).rejects.toBe('overlap');
  });

  it('a synchronous throw from invoke escapes the wrapper (wrappers are not async)', () => {
    invokeMock.mockImplementationOnce(() => {
      throw new Error('bridge down');
    });
    // The wrapper returns invoke's result directly, so a synchronous throw escapes the call.
    expect(() => editorApi.saveProject()).toThrow('bridge down');
  });
});

describe('event helpers', () => {
  type Captured = { name: string; handler: (e: { payload: unknown }) => void };
  let captured: Captured | undefined;
  let unlisten: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    captured = undefined;
    unlisten = vi.fn();
    listenMock.mockImplementation(async (name: string, handler: Captured['handler']) => {
      captured = { name, handler };
      return unlisten;
    });
  });

  it('onExportProgress listens to export-progress', async () => {
    await onExportProgress(() => {});
    expect(captured?.name).toBe('export-progress');
  });

  it('onExportDone listens to export-done', async () => {
    await onExportDone(() => {});
    expect(captured?.name).toBe('export-done');
  });

  it('onExportFailed listens to export-failed', async () => {
    await onExportFailed(() => {});
    expect(captured?.name).toBe('export-failed');
  });

  it('onExportQueue listens to export-queue', async () => {
    await onExportQueue(() => {});
    expect(captured?.name).toBe('export-queue');
  });

  it('onPeaksReady listens to peaks-ready', async () => {
    await onPeaksReady(() => {});
    expect(captured?.name).toBe('peaks-ready');
  });

  it('each helper passes two arguments to listen (name and handler)', async () => {
    listenMock.mockClear();
    await onExportProgress(() => {});
    expect(listenMock).toHaveBeenCalledTimes(1);
    expect(listenMock.mock.calls[0]?.length).toBe(2);
    expect(typeof listenMock.mock.calls[0]?.[1]).toBe('function');
  });

  it('onExportProgress resolves with the unlisten function from listen', async () => {
    await expect(onExportProgress(() => {})).resolves.toBe(unlisten);
  });

  it('onExportDone resolves with the unlisten function from listen', async () => {
    await expect(onExportDone(() => {})).resolves.toBe(unlisten);
  });

  it('onExportFailed resolves with the unlisten function from listen', async () => {
    await expect(onExportFailed(() => {})).resolves.toBe(unlisten);
  });

  it('onExportQueue resolves with the unlisten function from listen', async () => {
    await expect(onExportQueue(() => {})).resolves.toBe(unlisten);
  });

  it('onPeaksReady resolves with the unlisten function from listen', async () => {
    await expect(onPeaksReady(() => {})).resolves.toBe(unlisten);
  });

  it('the unlisten function can be called once the subscription resolves', async () => {
    const off = await onExportDone(() => {});
    off();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it('onExportProgress passes the event payload to the callback', async () => {
    const cb = vi.fn();
    await onExportProgress(cb);
    const job = { id: 'j1', percent: 42 };
    captured?.handler({ payload: job });
    expect(cb).toHaveBeenCalledWith(job);
  });

  it('onExportDone passes the event payload to the callback', async () => {
    const cb = vi.fn();
    await onExportDone(cb);
    captured?.handler({ payload: { id: 'j2', state: 'done' } });
    expect(cb).toHaveBeenCalledWith({ id: 'j2', state: 'done' });
  });

  it('onExportFailed passes the event payload to the callback', async () => {
    const cb = vi.fn();
    await onExportFailed(cb);
    captured?.handler({ payload: { id: 'j3', error: 'codec' } });
    expect(cb).toHaveBeenCalledWith({ id: 'j3', error: 'codec' });
  });

  it('onExportQueue passes the whole job array to the callback', async () => {
    const cb = vi.fn();
    await onExportQueue(cb);
    const jobs = [{ id: 'a' }, { id: 'b' }];
    captured?.handler({ payload: jobs });
    expect(cb).toHaveBeenCalledWith(jobs);
  });

  it('onPeaksReady passes mediaId and stream through', async () => {
    const cb = vi.fn();
    await onPeaksReady(cb);
    captured?.handler({ payload: { mediaId: 'm7', stream: 1 } });
    expect(cb).toHaveBeenCalledWith({ mediaId: 'm7', stream: 1 });
  });

  it('a listen failure rejects the helper promise', async () => {
    listenMock.mockRejectedValueOnce(new Error('no event bus'));
    await expect(onExportProgress(() => {})).rejects.toThrow('no event bus');
  });

  it('the callback is not invoked until the event fires', async () => {
    const cb = vi.fn();
    await onExportQueue(cb);
    expect(cb).not.toHaveBeenCalled();
  });
});

describe('exportStill and currentSnapshot', () => {
  it('exportStill sends null dir and nameTemplate when they are omitted', async () => {
    invokeMock.mockResolvedValueOnce('C:/out/still.png');
    await expect(editorApi.exportStill(1000, 'png', 95)).resolves.toBe('C:/out/still.png');
    expect(invokeMock).toHaveBeenCalledWith('editor_export_still', {
      at: 1000,
      format: 'png',
      quality: 95,
      dir: null,
      nameTemplate: null,
    });
  });

  it('exportStill treats explicit null dir and nameTemplate the same as omitted', async () => {
    await editorApi.exportStill(0, 'jpg', 70, null, null);
    expect(invokeMock).toHaveBeenCalledWith('editor_export_still', {
      at: 0,
      format: 'jpg',
      quality: 70,
      dir: null,
      nameTemplate: null,
    });
  });

  it('exportStill forwards a custom directory and name template verbatim', async () => {
    await editorApi.exportStill(250, 'png', 100, 'E:/stills', 'shot_{n}');
    const args = invokeMock.mock.calls[0][1] as Record<string, unknown>;
    expect(args.dir).toBe('E:/stills');
    expect(args.nameTemplate).toBe('shot_{n}');
  });

  it('exportStill sends exactly one invoke call per request', async () => {
    await editorApi.exportStill(1, 'png', 1);
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it('exportStill rejects when the backend fails', async () => {
    invokeMock.mockRejectedValueOnce('no frame at that time');
    await expect(editorApi.exportStill(1, 'png', 1)).rejects.toBe('no frame at that time');
  });

  it('currentSnapshot uses the editor_current_snapshot command with no arguments', async () => {
    invokeMock.mockResolvedValueOnce(null);
    await expect(editorApi.currentSnapshot()).resolves.toBeNull();
    expect(invokeMock).toHaveBeenCalledWith('editor_current_snapshot');
    expect(invokeMock.mock.calls[0]).toHaveLength(1);
  });

  it('currentSnapshot resolves with the snapshot object the backend returns', async () => {
    const snap = { revision: 7 } as unknown as Awaited<ReturnType<typeof editorApi.currentSnapshot>>;
    invokeMock.mockResolvedValueOnce(snap);
    await expect(editorApi.currentSnapshot()).resolves.toBe(snap);
  });

  it('currentSnapshot propagates a backend error', async () => {
    invokeMock.mockRejectedValueOnce('no open project');
    await expect(editorApi.currentSnapshot()).rejects.toBe('no open project');
  });

  it('currentSnapshot does not send a snake_case command name', async () => {
    await editorApi.currentSnapshot();
    const cmd = invokeMock.mock.calls[0][0] as string;
    expect(cmd).not.toMatch(/currentSnapshot|current-snapshot/);
    expect(cmd).toBe('editor_current_snapshot');
  });

  it('exportStill and currentSnapshot are both present on editorApi', () => {
    expect(typeof editorApi.exportStill).toBe('function');
    expect(typeof editorApi.currentSnapshot).toBe('function');
  });
});
