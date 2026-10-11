<script lang="ts">
  import { dbToMeterFraction, METER_FLOOR_DB, METER_CEIL_DB } from '../../discovery';

  interface Props {
    /** Level in dBFS, -100..0. */
    db: number;
    /** Optional marker, such as a noise-gate threshold. */
    thresholdDb?: number | null;
    label?: string;
    height?: number;
  }

  let { db, thresholdDb = null, label = 'Input level', height = 6 }: Props = $props();

  const fillPct = $derived(dbToMeterFraction(db) * 100);
  const markPct = $derived(thresholdDb === null ? null : dbToMeterFraction(thresholdDb) * 100);
  const nowDb = $derived(Number.isFinite(db) ? Math.min(METER_CEIL_DB, Math.max(METER_FLOOR_DB, db)) : METER_FLOOR_DB);
  /** Warn colours near the top of the range. */
  const level = $derived(nowDb > -1 ? 'hot' : nowDb > -6 ? 'warm' : 'ok');
</script>

<div
  class="meter"
  role="meter"
  aria-label={label}
  aria-valuemin={METER_FLOOR_DB}
  aria-valuemax={METER_CEIL_DB}
  aria-valuenow={Math.round(nowDb)}
  aria-valuetext="{Math.round(nowDb)} dB"
  style:height="{height}px"
>
  <div class="fill {level}" style:width="{fillPct}%"></div>
  {#if markPct !== null}
    <div class="mark" style:left="{markPct}%" aria-hidden="true"></div>
  {/if}
</div>

<style>
  .meter {
    position: relative;
    width: 100%;
    overflow: visible;
    border-radius: 999px;
    background: var(--surface-2);
    border: 1px solid var(--border);
  }

  .fill {
    height: 100%;
    border-radius: 999px;
    background: #3ddc84;
    transition: width 80ms linear;
  }

  .fill.warm {
    background: #f5c542;
  }

  .fill.hot {
    background: var(--accent);
  }

  .mark {
    position: absolute;
    top: -3px;
    bottom: -3px;
    width: 2px;
    margin-left: -1px;
    background: var(--text);
    border-radius: 1px;
  }

  @media (prefers-reduced-motion: reduce) {
    .fill {
      transition: none;
    }
  }
</style>
