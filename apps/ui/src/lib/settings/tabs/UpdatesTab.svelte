<script lang="ts">
  import { onMount } from 'svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import { formatRelativeDate } from '../../format';
  import { checkNow, updateStore, type UpdateInfo } from '../../updates';
  import { isTauriRuntime } from '../../window';
  import FieldRow from '../fields/FieldRow.svelte';
  import NumberField from '../fields/NumberField.svelte';
  import Select from '../fields/Select.svelte';
  import { settings, setPath } from '../store';

  interface Props {
    disabled?: boolean;
  }

  let { disabled = false }: Props = $props();

  /** The `[updates]` section, read from the draft (kept as opaque JSON by the store). */
  interface UpdatesDraft {
    check?: 'never' | 'notify';
    interval_hours?: number;
  }
  const updates = $derived($settings.draft?.updates as UpdatesDraft | undefined);

  const CHECK_OPTIONS = [
    { value: 'notify', label: 'Check automatically and notify me' },
    { value: 'never', label: 'Never check automatically' },
  ];

  const INTERVAL_MIN = 1;
  const INTERVAL_MAX = 168;

  let version = $state('');
  let checking = $state(false);
  /** Outcome of the last Check now: an update, up to date, or a failure. */
  let result = $state<{ kind: 'up-to-date' } | { kind: 'update'; info: UpdateInfo } | { kind: 'error'; message: string } | null>(null);

  onMount(() => {
    if (!isTauriRuntime()) return;
    getVersion()
      .then((v) => (version = v))
      .catch(() => (version = 'unknown'));
  });

  const lastChecked = $derived.by(() => {
    const ms = $updateStore.state?.last_checked_unix_ms;
    return ms ? formatRelativeDate(ms) : 'Never';
  });

  const resultText = $derived.by(() => {
    if (!result) return '';
    if (result.kind === 'up-to-date') return 'You are up to date.';
    if (result.kind === 'update') return `Update ${result.info.version} available.`;
    return `Check failed: ${result.message}`;
  });

  async function onCheckNow() {
    checking = true;
    result = null;
    try {
      const info = await checkNow();
      result = info ? { kind: 'update', info } : { kind: 'up-to-date' };
    } catch (e) {
      result = { kind: 'error', message: e instanceof Error ? e.message : String(e) };
    } finally {
      checking = false;
    }
  }
</script>

<h2>Updates</h2>

<FieldRow label="Current version" path="updates.version_info">
  {#snippet children()}
    <span class="value">{version || '...'}</span>
  {/snippet}
</FieldRow>

<FieldRow label="Last checked" path="updates.last_checked">
  {#snippet children()}
    <span class="value">{lastChecked}</span>
  {/snippet}
</FieldRow>

<FieldRow label="Check for updates now" path="updates.check_now" help="Contacts the update server once. Works even when automatic checks are off.">
  {#snippet children()}
    <div class="check">
      <button type="button" class="check-button" disabled={checking} onclick={() => void onCheckNow()}>
        {checking ? 'Checking...' : 'Check now'}
      </button>
      {#if resultText}
        <p class="result" role="status">{resultText}</p>
      {/if}
    </div>
  {/snippet}
</FieldRow>

{#if updates}
  <FieldRow
    label="Automatic checks"
    path="updates.check"
    help="Checks run on launch and then on the interval below."
  >
    {#snippet children(ctl)}
      <Select
        {...ctl}
        value={updates.check ?? 'notify'}
        options={CHECK_OPTIONS}
        {disabled}
        onchange={(v) => setPath('updates.check', v)}
      />
    {/snippet}
  </FieldRow>

  {#if (updates.check ?? 'notify') === 'notify'}
    <FieldRow
      label="Check every"
      path="updates.interval_hours"
      help="Hours between automatic checks, from 1 to 168."
    >
      {#snippet children(ctl)}
        <NumberField
          {...ctl}
          value={updates.interval_hours ?? 12}
          min={INTERVAL_MIN}
          max={INTERVAL_MAX}
          unit="hours"
          {disabled}
          onchange={(v) => setPath('updates.interval_hours', v)}
        />
      {/snippet}
    </FieldRow>
  {/if}
{/if}

<style>
  .value {
    font-size: 13px;
    color: var(--text);
  }

  .check {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 8px;
  }

  .check-button {
    padding: 7px 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--surface-2);
    color: var(--text);
    font: inherit;
    font-size: 13px;
    cursor: pointer;
  }

  .check-button:hover:not(:disabled) {
    border-color: var(--text-dim);
  }

  .check-button:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .result {
    margin: 0;
    font-size: 13px;
    color: var(--text-dim);
  }
</style>
