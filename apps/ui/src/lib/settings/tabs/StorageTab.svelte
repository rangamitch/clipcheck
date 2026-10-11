<script lang="ts">
  import FieldRow from '../fields/FieldRow.svelte';
  import NumberField from '../fields/NumberField.svelte';
  import PathField from '../fields/PathField.svelte';
  import Select from '../fields/Select.svelte';
  import TextField from '../fields/TextField.svelte';
  import Toggle from '../fields/Toggle.svelte';
  import { QUOTA_GB_MAX } from '../bounds';
  import { formatFilenameExample } from '../pure';
  import { settings, setPath, type QuotaAction } from '../store';

  interface Props {
    disabled?: boolean;
  }

  let { disabled = false }: Props = $props();

  const storage = $derived($settings.draft?.storage);
  const example = $derived(
    storage ? formatFilenameExample(storage.filename_template, new Date()) : '',
  );

  const QUOTA_OPTIONS: { value: QuotaAction; label: string }[] = [
    { value: 'warn', label: 'Warn me only' },
    { value: 'delete_oldest_clips', label: 'Delete the oldest clips' },
    { value: 'stop_recording', label: 'Stop recording' },
  ];
</script>

{#if storage}
  <h2>Storage</h2>

  <FieldRow
    label="Recordings folder"
    path="storage.root"
    help="Where clips and sessions are saved. Windows variables such as %USERPROFILE% are expanded."
  >
    {#snippet children(ctl)}
      <PathField
        {...ctl}
        value={storage.root}
        showOpen
        {disabled}
        onchange={(v) => setPath('storage.root', v)}
      />
    {/snippet}
  </FieldRow>

  <FieldRow
    label="Save each game in its own folder"
    path="storage.per_game_folders"
    help="Creates a folder for each game inside the recordings folder."
  >
    {#snippet children(ctl)}
      <Toggle
        {...ctl}
        checked={storage.per_game_folders}
        {disabled}
        onchange={(v) => setPath('storage.per_game_folders', v)}
      />
    {/snippet}
  </FieldRow>

  <FieldRow
    label="File name"
    path="storage.filename_template"
    help={`Tokens: {game}, {kind}, {id} and date tokens such as {yyyy-MM-dd HH-mm-ss}. Example: ${example}`}
  >
    {#snippet children(ctl)}
      <TextField
        {...ctl}
        value={storage.filename_template}
        {disabled}
        onchange={(v) => setPath('storage.filename_template', v)}
      />
    {/snippet}
  </FieldRow>

  <FieldRow
    label="Storage limit"
    path="storage.quota_gb"
    help="The most space recordings may use. 0 means no limit."
  >
    {#snippet children(ctl)}
      <NumberField
        {...ctl}
        value={storage.quota_gb}
        min={0}
        max={QUOTA_GB_MAX}
        step={1}
        unit="GB"
        {disabled}
        onchange={(v) => setPath('storage.quota_gb', v)}
      />
    {/snippet}
  </FieldRow>

  <FieldRow
    label="When the limit is reached"
    path="storage.quota_action"
    help="Applies only when a storage limit is set."
  >
    {#snippet children(ctl)}
      <Select
        {...ctl}
        value={storage.quota_action}
        options={QUOTA_OPTIONS}
        {disabled}
        onchange={(v) => setPath('storage.quota_action', v)}
      />
    {/snippet}
  </FieldRow>

  <FieldRow
    label="Send deleted recordings to the Recycle Bin"
    path="storage.delete_to_recycle_bin"
    help="When off, deleted recordings are removed permanently."
  >
    {#snippet children(ctl)}
      <Toggle
        {...ctl}
        checked={storage.delete_to_recycle_bin}
        {disabled}
        onchange={(v) => setPath('storage.delete_to_recycle_bin', v)}
      />
    {/snippet}
  </FieldRow>
{/if}
