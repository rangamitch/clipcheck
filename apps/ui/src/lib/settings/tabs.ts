/**
 * Registry of Settings tabs, in display order. To add a tab, import its component and set
 * `component` (required, so every tab in the list renders).
 *
 * Every tab component takes `disabled` (true while the recorder is offline or saving) and
 * reads and writes the draft through `settings` and `setPath` from `./store`.
 */
import type { Component } from 'svelte';
import { writable } from 'svelte/store';
import AudioTab from './tabs/AudioTab.svelte';
import EditorSettingsPanel from '../editor/EditorSettingsPanel.svelte';
import DetectionTab from './tabs/DetectionTab.svelte';
import GeneralTab from './tabs/GeneralTab.svelte';
import HotkeysTab from './tabs/HotkeysTab.svelte';
import ReplayTab from './tabs/ReplayTab.svelte';
import StorageTab from './tabs/StorageTab.svelte';
import UpdatesTab from './tabs/UpdatesTab.svelte';
import VideoTab from './tabs/VideoTab.svelte';

export interface SettingsTabProps {
  disabled: boolean;
}

export interface SettingsTab {
  id: string;
  label: string;
  component: Component<SettingsTabProps>;
}

export const SETTINGS_TABS: SettingsTab[] = [
  { id: 'general', label: 'General', component: GeneralTab },
  { id: 'storage', label: 'Storage', component: StorageTab },
  { id: 'detection', label: 'Games', component: DetectionTab },
  { id: 'video', label: 'Video', component: VideoTab },
  { id: 'editor', label: 'Video editor', component: EditorSettingsPanel },
  { id: 'audio', label: 'Audio', component: AudioTab },
  { id: 'replay', label: 'Recordings', component: ReplayTab },
  { id: 'hotkeys', label: 'Hotkeys', component: HotkeysTab },
  { id: 'updates', label: 'Updates', component: UpdatesTab },
];

/** The tab id requested by `selectTab`, or null. Settings.svelte shows it and clears nothing. */
export const requestedTab = writable<string | null>(null);

/** Asks the Settings screen to show a tab, such as a link from another tab. */
export function selectTab(id: string): void {
  if (SETTINGS_TABS.some((t) => t.id === id)) requestedTab.set(id);
}
