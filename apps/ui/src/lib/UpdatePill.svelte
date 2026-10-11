<script lang="ts">
  import { onMount } from 'svelte';
  import { initUpdates, isPillVisible, openUpdateModal, updateStore } from './updates';

  onMount(() => {
    const stop = initUpdates();
    return () => {
      void stop.then((fn) => fn());
    };
  });
</script>

<!-- Shown in the TitleBar status slot only when an update is available and not skipped. -->
{#if isPillVisible($updateStore.available)}
  <button
    type="button"
    class="pill"
    title="Version {$updateStore.available?.version} is ready. Open update details"
    onclick={openUpdateModal}
  >
    ↑ New Update Available
  </button>
{/if}

<style>
  .pill {
    flex: 0 0 auto;
    height: 24px;
    padding: 0 10px;
    border: 1px solid var(--accent);
    border-radius: 999px;
    background: var(--surface-2);
    color: var(--accent);
    font: inherit;
    font-size: 12px;
    white-space: nowrap;
    cursor: pointer;
  }

  .pill:hover {
    background: var(--surface);
  }

  .pill:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
