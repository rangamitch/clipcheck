/**
 * Logs and diagnostics helpers for the Settings "Logs & diagnostics" panel.
 *
 * Pure helpers (`parentDir`, `friendlyError`) are exported for unit tests. The wrappers call
 * the Tauri commands implemented in apps/ui/src-tauri. Outside Tauri, `invoke` rejects.
 */
import { invoke } from '@tauri-apps/api/core';

const DEFAULT_ERROR = 'Something went wrong. Try again.';

/**
 * Returns the folder that contains `path`, keeping the separator style of the input.
 * Handles Windows (`C:\a\b.zip`) and POSIX (`/a/b.zip`) paths. Returns '' when there is no
 * separator. A file directly under a drive root returns the root (`C:\`).
 */
export function parentDir(path: string): string {
  const trimmed = path.trim();
  const idx = Math.max(trimmed.lastIndexOf('\\'), trimmed.lastIndexOf('/'));
  if (idx < 0) return '';
  if (idx === 0) return trimmed.slice(0, 1);
  const head = trimmed.slice(0, idx);
  // "C:" alone is a drive, not a folder: keep its root separator.
  if (/^[A-Za-z]:$/.test(head)) return head + trimmed[idx];
  return head;
}

/**
 * Turns a rejected invoke or a thrown value into a short message for the user. Tauri commands
 * reject with a plain string, so strings are used as-is.
 */
export function friendlyError(err: unknown, fallback: string = DEFAULT_ERROR): string {
  if (typeof err === 'string') return err.trim() || fallback;
  if (err instanceof Error) return err.message.trim() || fallback;
  if (err && typeof err === 'object' && 'message' in err) {
    const message = (err as { message: unknown }).message;
    if (typeof message === 'string' && message.trim()) return message.trim();
  }
  return fallback;
}

/** Absolute path of the folder that holds the ClipCheck log files. */
export async function getLogDir(): Promise<string> {
  const dir = await invoke<string>('get_log_dir');
  if (!dir) throw new Error('The log folder could not be found.');
  return dir;
}

/** Recent log plus system info as one text block, with the Windows user name hidden. */
export function collectDiagnostics(): Promise<string> {
  return invoke<string>('collect_diagnostics');
}

/** Writes the diagnostics to a file and resolves to its absolute path. */
export async function exportDiagnostics(): Promise<string> {
  const path = await invoke<string>('export_diagnostics');
  if (!path) throw new Error('The diagnostics file could not be saved.');
  return path;
}

/** Opens a folder in Explorer. */
export function openFolder(path: string): Promise<void> {
  return invoke<void>('open_folder', { path });
}

/** Copies text to the system clipboard. Throws when the clipboard API is unavailable. */
export async function copyText(text: string): Promise<void> {
  if (typeof navigator === 'undefined' || !navigator.clipboard) {
    throw new Error('The clipboard is not available here.');
  }
  await navigator.clipboard.writeText(text);
}
