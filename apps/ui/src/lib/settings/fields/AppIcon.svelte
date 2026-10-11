<script lang="ts">
  import { iconSrc, fallbackColor, initialOf } from '../../discovery';

  interface Props {
    exe: string;
    name?: string;
    iconB64?: string | null;
    size?: number;
  }

  let { exe, name = '', iconB64 = null, size = 24 }: Props = $props();

  const src = $derived(iconSrc(iconB64));
  const letter = $derived(initialOf(name, exe));
  const bg = $derived(fallbackColor(exe));
</script>

{#if src}
  <img class="icon" src={src} alt="" aria-hidden="true" style:width="{size}px" style:height="{size}px" />
{:else}
  <span
    class="icon fallback"
    aria-hidden="true"
    style:background={bg}
    style:width="{size}px"
    style:height="{size}px"
    style:font-size="{Math.round(size * 0.5)}px">{letter}</span
  >
{/if}

<style>
  .icon {
    flex: none;
    display: inline-grid;
    place-items: center;
    border-radius: 6px;
    object-fit: contain;
  }

  .fallback {
    color: #fff;
    font-weight: 600;
    line-height: 1;
    user-select: none;
  }
</style>
