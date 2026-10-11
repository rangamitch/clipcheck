import { describe, expect, it } from 'vitest';
import type { DiagnosticDto } from '../daemon';
import { diagnosticAt, fieldDiagnostic, hasBlockingErrors } from './pure';

const d = (path: string, severity: 'error' | 'warning', code = `code.${path}`): DiagnosticDto => ({
  code,
  severity,
  path,
  message: `${severity}: ${path}`,
});

describe('audio track row errors', () => {
  const list = [d('audio.tracks[2].gain_db', 'error', 'audio.gain')];

  it('shows on the row it belongs to', () => {
    expect(fieldDiagnostic('audio.tracks[2].gain_db', list)).toEqual({
      code: 'audio.gain',
      severity: 'error',
      message: 'error: audio.tracks[2].gain_db',
    });
  });

  it('does not show on a neighbouring row', () => {
    expect(fieldDiagnostic('audio.tracks[1].gain_db', list)).toBeNull();
    expect(fieldDiagnostic('audio.tracks[3].gain_db', list)).toBeNull();
  });

  it('does not show on another field of the same row', () => {
    expect(fieldDiagnostic('audio.tracks[2].name', list)).toBeNull();
  });

  it('does not match an index with the same leading digits', () => {
    expect(fieldDiagnostic('audio.tracks[20].gain_db', list)).toBeNull();
    expect(fieldDiagnostic('audio.tracks[1]', [d('audio.tracks[10].name', 'error')])).toBeNull();
  });
});

describe('list-level fieldDiagnostic matches rows below it (prefix)', () => {
  it('fieldDiagnostic("audio.tracks") matches a row error', () => {
    expect(fieldDiagnostic('audio.tracks', [d('audio.tracks[0].name', 'error')])?.severity).toBe('error');
  });

  it('fieldDiagnostic("audio") matches a nested row error through the dot', () => {
    expect(fieldDiagnostic('audio', [d('audio.tracks[0].gain_db', 'warning')])?.severity).toBe('warning');
  });

  it('fieldDiagnostic("hotkeys.clips") matches an indexed clip error', () => {
    expect(fieldDiagnostic('hotkeys.clips', [d('hotkeys.clips[1].seconds', 'error')])).not.toBeNull();
  });

  it('fieldDiagnostic("video.preset") does not match video.presets or video.preset_x', () => {
    const list = [d('video.presets', 'error'), d('video.preset_x', 'error')];
    expect(fieldDiagnostic('video.preset', list)).toBeNull();
  });

  it('fieldDiagnostic("storage.root") does not match storage.root2', () => {
    expect(fieldDiagnostic('storage.root', [d('storage.root2', 'error')])).toBeNull();
  });
});

describe('diagnosticAt does not match rows below the list', () => {
  it('diagnosticAt("audio.tracks") does not match audio.tracks[0].name', () => {
    expect(diagnosticAt('audio.tracks', [d('audio.tracks[0].name', 'error')])).toBeNull();
  });

  it('diagnosticAt("audio.tracks") matches the list itself', () => {
    expect(diagnosticAt('audio.tracks', [d('audio.tracks', 'error', 'audio.count')])?.code).toBe('audio.count');
  });

  it('diagnosticAt matches the row path exactly', () => {
    expect(diagnosticAt('audio.tracks[0].name', [d('audio.tracks[0].name', 'error')])).not.toBeNull();
  });
});

describe('clip hotkey errors map to the clip path', () => {
  const list = [d('hotkeys.clips[1].seconds', 'error', 'clip.seconds')];

  it('fieldDiagnostic on the seconds field of clip 1 finds the error', () => {
    expect(fieldDiagnostic('hotkeys.clips[1].seconds', list)?.code).toBe('clip.seconds');
  });

  it('diagnosticAt on the seconds field of clip 1 finds the error', () => {
    expect(diagnosticAt('hotkeys.clips[1].seconds', list)?.code).toBe('clip.seconds');
  });

  it('clip 0 has no error', () => {
    expect(fieldDiagnostic('hotkeys.clips[0].seconds', list)).toBeNull();
    expect(diagnosticAt('hotkeys.clips[0].seconds', list)).toBeNull();
  });

  it('the list-level diagnosticAt does not pick up the row error', () => {
    expect(diagnosticAt('hotkeys.clips', list)).toBeNull();
  });

  it('the list-level fieldDiagnostic does pick up the row error', () => {
    expect(fieldDiagnostic('hotkeys.clips', list)?.code).toBe('clip.seconds');
  });
});

describe('error beats warning across descendant paths', () => {
  it('an error on a row beats a warning on the list path', () => {
    const list = [d('audio.tracks', 'warning', 'w'), d('audio.tracks[1].gain_db', 'error', 'e')];
    expect(fieldDiagnostic('audio.tracks', list)).toEqual({
      code: 'e',
      severity: 'error',
      message: 'error: audio.tracks[1].gain_db',
    });
  });

  it('a warning alone is returned as a warning', () => {
    expect(fieldDiagnostic('audio.tracks', [d('audio.tracks[0].name', 'warning')])?.severity).toBe('warning');
  });

  it('no matches gives null', () => {
    expect(fieldDiagnostic('audio.tracks', [d('video.preset', 'error')])).toBeNull();
    expect(fieldDiagnostic('audio.tracks', [])).toBeNull();
  });
});

describe('hasBlockingErrors', () => {
  it('false for an empty list', () => {
    expect(hasBlockingErrors([])).toBe(false);
  });

  it('false for warnings only, since warnings never block a save', () => {
    expect(hasBlockingErrors([d('a', 'warning'), d('b', 'warning')])).toBe(false);
  });

  it('true when any error is present', () => {
    expect(hasBlockingErrors([d('a', 'warning'), d('b', 'error')])).toBe(true);
  });
});
