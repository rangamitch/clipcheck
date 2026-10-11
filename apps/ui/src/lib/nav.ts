import { writable } from 'svelte/store';
import type { RecordingDto } from './api';

/** Top-level views the main window can show. */
export const VIEWS = ['gallery', 'settings'] as const;
export type View = (typeof VIEWS)[number];

/** Currently active view. Starts on the gallery. */
export const activeView = writable<View>('gallery');

/** Id of the recording open in the player, or null when the player is closed. */
export const playerTarget = writable<string | null>(null);

/** File name of the recording open in the player, or null when unknown or the player is closed. */
export const playerFileName = writable<string | null>(null);

/**
 * The Gallery row the player was opened from (D60): lets the player render name, size, game and
 * date instantly. Null when closed or when opened by id only.
 */
export const playerRow = writable<RecordingDto | null>(null);

/** Switches the active view. Any open clip is left first (D60), so the rail always navigates. */
export function setView(view: View): void {
  closePlayer();
  activeView.set(view);
}

/** Opens the player for a recording. Pass the whole row for an instant start. */
export function openRecording(target: string | RecordingDto, fileName?: string): void {
  if (typeof target === 'string') {
    playerTarget.set(target);
    playerFileName.set(fileName ?? null);
    playerRow.set(null);
    return;
  }
  playerTarget.set(target.id);
  playerFileName.set(fileName ?? target.fileName);
  playerRow.set(target);
}

/** Merges a change (rename, favourite) into the open row and keeps the file name in step. */
export function updatePlayerRow(patch: Partial<RecordingDto>): void {
  playerRow.update((row) => (row === null ? row : { ...row, ...patch }));
  if (patch.fileName !== undefined) playerFileName.set(patch.fileName);
}

/** Closes the player and returns to the current view. */
export function closePlayer(): void {
  playerTarget.set(null);
  playerFileName.set(null);
  playerRow.set(null);
}
