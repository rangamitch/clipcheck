<script module lang="ts">
// Inline SVG icons for the player controls. Lucide-style paths, drawn by hand.
// Stroked icons use stroke="currentColor"; play, pause and star-filled are solid.

export type IconName =
  | 'play'
  | 'pause'
  | 'volume'
  | 'volume-low'
  | 'mute'
  | 'speed'
  | 'fullscreen'
  | 'fullscreen-exit'
  | 'folder'
  | 'star'
  | 'star-filled'
  | 'back'
  | 'scissors'
  | 'audio'
  | 'check'
  | 'close'
  | 'reset'
  | 'zoom-in'
  | 'zoom-out';
</script>

<script lang="ts">
  let { name, size = 20 }: { name: IconName; size?: number } = $props();
</script>

<svg
  viewBox="0 0 24 24"
  width={size}
  height={size}
  fill="none"
  stroke="currentColor"
  stroke-width="2"
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"
  focusable="false"
  class="icon"
>
  {#if name === 'play'}
    <polygon points="7 4 20 12 7 20 7 4" fill="currentColor" />
  {:else if name === 'pause'}
    <rect x="6" y="4" width="4" height="16" rx="1" fill="currentColor" stroke="none" />
    <rect x="14" y="4" width="4" height="16" rx="1" fill="currentColor" stroke="none" />
  {:else if name === 'volume'}
    <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
    <path d="M15.54 8.46a5 5 0 0 1 0 7.07" />
    <path d="M19.07 4.93a10 10 0 0 1 0 14.14" />
  {:else if name === 'volume-low'}
    <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
    <path d="M15.54 8.46a5 5 0 0 1 0 7.07" />
  {:else if name === 'mute'}
    <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
    <path d="M22 9l-6 6" />
    <path d="M16 9l6 6" />
  {:else if name === 'speed'}
    <path d="M3 16a9 9 0 1 1 18 0" />
    <path d="M12 16l4-5" />
  {:else if name === 'fullscreen'}
    <path d="M4 9V4h5" />
    <path d="M20 9V4h-5" />
    <path d="M4 15v5h5" />
    <path d="M20 15v5h-5" />
  {:else if name === 'fullscreen-exit'}
    <path d="M9 4v5H4" />
    <path d="M15 4v5h5" />
    <path d="M9 20v-5H4" />
    <path d="M15 20v-5h5" />
  {:else if name === 'folder'}
    <path
      d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"
    />
  {:else if name === 'star'}
    <polygon points="12 3 14.7 8.6 20.8 9.5 16.4 13.8 17.5 19.9 12 17 6.5 19.9 7.6 13.8 3.2 9.5 9.3 8.6 12 3" />
  {:else if name === 'star-filled'}
    <polygon
      points="12 3 14.7 8.6 20.8 9.5 16.4 13.8 17.5 19.9 12 17 6.5 19.9 7.6 13.8 3.2 9.5 9.3 8.6 12 3"
      fill="currentColor"
    />
  {:else if name === 'back'}
    <path d="M15 18l-6-6 6-6" />
  {:else if name === 'scissors'}
    <circle cx="6" cy="6" r="3" />
    <circle cx="6" cy="18" r="3" />
    <path d="M20 4L8.12 15.88" />
    <path d="M14.47 14.48L20 20" />
    <path d="M8.12 8.12L12 12" />
  {:else if name === 'audio'}
    <polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
    <path d="M15.54 8.46a5 5 0 0 1 0 7.07" />
  {:else if name === 'check'}
    <path d="M20 6L9 17l-5-5" />
  {:else if name === 'close'}
    <path d="M18 6L6 18" />
    <path d="M6 6l12 12" />
  {:else if name === 'reset'}
    <path d="M3 12a9 9 0 1 0 3-6.7" />
    <path d="M3 4v5h5" />
  {:else if name === 'zoom-in'}
    <circle cx="11" cy="11" r="7" />
    <path d="M21 21l-4.3-4.3" />
    <path d="M11 8v6" />
    <path d="M8 11h6" />
  {:else if name === 'zoom-out'}
    <circle cx="11" cy="11" r="7" />
    <path d="M21 21l-4.3-4.3" />
    <path d="M8 11h6" />
  {/if}
</svg>

<style>
  .icon {
    display: block;
    flex: none;
  }
</style>
