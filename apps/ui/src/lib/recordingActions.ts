/**
 * Recording management actions shared by the Gallery, the Player and context menus.
 *
 * Daemon commands go through `request` (daemon.ts). Shell actions go through Tauri `invoke`
 * (apps/ui/src-tauri/src/fileops.rs). Outside Tauri, shell actions are no-ops and `pickFolder`
 * resolves to `null`.
 */
import { invoke } from '@tauri-apps/api/core';
import { request, isErrorReply, type Reply } from './daemon';
import { isTauriRuntime } from './window';
import { openEditor } from './nav';

/** Turns a daemon error reply into a readable Error. */
function replyError(reply: Extract<Reply, { type: 'error' }>): Error {
  const { code, message } = reply.data;
  switch (code) {
    case 'busy':
      return new Error('The recorder is busy with another change. Try again in a moment.');
    case 'bad_request':
      return new Error(message || 'That name cannot be used.');
    case 'shutting_down':
      return new Error('ClipCheck is shutting down.');
    case 'invalid_state':
      return new Error(message || 'This recording cannot be changed right now.');
    default:
      return new Error(message || `The request failed (${code}).`);
  }
}

/** Sends a daemon command that answers `ok` on success. */
async function expectOk(command: Parameters<typeof request>[0]): Promise<void> {
  const reply = await request(command);
  if (isErrorReply(reply)) throw replyError(reply);
  if (reply.type !== 'ok') throw new Error('Unexpected reply from the recorder.');
}

/**
 * Renames a recording. `name` is the new stem without extension; the daemon keeps the
 * original extension. Throws an Error with a readable message on failure.
 */
export function renameRecording(id: string, name: string): Promise<void> {
  return expectOk({ type: 'rename_recording', data: { recording_id: id, name } });
}

/** Deletes a recording. The daemon moves the file to the Recycle Bin. Throws on failure. */
export function deleteRecording(id: string): Promise<void> {
  return expectOk({ type: 'delete_recording', data: { recording_id: id } });
}

/** Opens Explorer with the recording's file selected. No-op outside Tauri. */
export async function revealRecording(id: string): Promise<void> {
  if (!isTauriRuntime()) return;
  await invoke<void>('reveal_recording', { id });
}

/** Opens a folder in Explorer. No-op outside Tauri. */
export async function openFolder(path: string): Promise<void> {
  if (!isTauriRuntime()) return;
  await invoke<void>('open_folder', { path });
}

/**
 * Shows the native folder picker. Resolves to the chosen path, or `null` when the user
 * cancels or the app is not running inside Tauri.
 */
export async function pickFolder(initial?: string): Promise<string | null> {
  if (!isTauriRuntime()) return null;
  return invoke<string | null>('pick_folder', { initial: initial ?? null });
}

/**
 * Opens the video editor on a new project built from one recording. `range` (ms, source time)
 * is the player trim selection; omit it to place the full recording.
 */
export function editInEditor(id: string, range?: { startMs: number; endMs: number }): void {
  openEditor({ projectId: null, recordingIds: id ? [id] : [], ...(range ? { range } : {}) });
}

/** Sets or clears the favourite flag of a recording. Throws on failure. */
export function setFavorite(id: string, favorite: boolean): Promise<void> {
  return expectOk({ type: 'set_favorite', data: { recording_id: id, favorite } });
}
