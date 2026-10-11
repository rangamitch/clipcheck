<script lang="ts">
  import { onMount } from 'svelte';
  import type { TrackMixer } from './mixer';
  import {
    DEFAULT_OUTPUT_ID,
    SYSTEM_DEFAULT_LABEL,
    getSavedOutput,
    listOutputDevices,
    saveOutput,
    type OutputDevice,
  } from './output';

  interface Props {
    mixer: TrackMixer;
  }

  let { mixer }: Props = $props();

  let devices = $state<OutputDevice[]>([{ id: DEFAULT_OUTPUT_ID, label: SYSTEM_DEFAULT_LABEL }]);
  let selected = $state(getSavedOutput());
  let failed = $state(false);

  // A saved device that is not connected stays visible so the choice is not silently lost.
  const options = $derived(
    devices.some((d) => d.id === selected)
      ? devices
      : [...devices, { id: selected, label: 'Saved device (not connected)' }],
  );

  async function refresh() {
    devices = await listOutputDevices();
  }

  async function onSelect(event: Event) {
    const id = (event.currentTarget as HTMLSelectElement).value;
    selected = id;
    saveOutput(id);
    failed = !(await mixer.setOutputDevice(id));
  }

  onMount(() => {
    let alive = true;
    const md = typeof navigator !== 'undefined' ? navigator.mediaDevices : undefined;
    const onDeviceChange = () => {
      void listOutputDevices().then((list) => {
        if (alive) devices = list;
      });
    };
    void refresh();
    md?.addEventListener?.('devicechange', onDeviceChange);
    return () => {
      alive = false;
      md?.removeEventListener?.('devicechange', onDeviceChange);
    };
  });
</script>

<div class="output-picker">
  <label for="player-audio-output">Audio output</label>
  <select id="player-audio-output" value={selected} onchange={onSelect}>
    {#each options as device (device.id)}
      <option value={device.id}>{device.label}</option>
    {/each}
  </select>
  {#if failed}
    <span class="error" role="status">Could not switch to this device</span>
  {/if}
</div>

<style>
  .output-picker {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.85rem;
  }

  select {
    max-width: 16rem;
    padding: 0.15rem 0.3rem;
  }

  .error {
    color: var(--danger, #c0392b);
  }
</style>
