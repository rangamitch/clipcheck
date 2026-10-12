<script lang="ts">
  import type { Inline } from './markdown';
  import InlineMd from './InlineMd.svelte';

  let { nodes }: { nodes: Inline[] } = $props();
</script>

{#each nodes as n}
  {#if n.t === 'text'}{n.v}{:else if n.t === 'code'}<code>{n.v}</code>{:else if n.t === 'bold'}<strong
      ><InlineMd nodes={n.c} /></strong
    >{:else if n.t === 'italic'}<em><InlineMd nodes={n.c} /></em>{:else}<s><InlineMd nodes={n.c} /></s>{/if}
{/each}
