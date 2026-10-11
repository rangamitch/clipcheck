<script lang="ts">
  import FieldRow from '../fields/FieldRow.svelte';
  import Segmented from '../fields/Segmented.svelte';
  import Toggle from '../fields/Toggle.svelte';
  import { settings, setPath, type CloseBehavior } from '../store';

  interface Props {
    disabled?: boolean;
  }

  let { disabled = false }: Props = $props();

  const general = $derived($settings.draft?.general);

  const CLOSE_OPTIONS: { value: CloseBehavior; label: string }[] = [
    { value: 'minimize_to_tray', label: 'Minimize to tray' },
    { value: 'exit', label: 'Exit ClipCheck' },
  ];
</script>

{#if general}
  <h2>General</h2>

  <FieldRow
    label="Start with Windows"
    path="general.autostart"
    help="Open ClipCheck when you sign in to Windows."
  >
    {#snippet children(ctl)}
      <Toggle
        {...ctl}
        checked={general.autostart}
        {disabled}
        onchange={(v) => setPath('general.autostart', v)}
      />
    {/snippet}
  </FieldRow>

  <FieldRow
    label="Start minimized"
    path="general.start_minimized"
    help="Open the window hidden in the tray when ClipCheck starts."
  >
    {#snippet children(ctl)}
      <Toggle
        {...ctl}
        checked={general.start_minimized}
        {disabled}
        onchange={(v) => setPath('general.start_minimized', v)}
      />
    {/snippet}
  </FieldRow>

  <FieldRow
    label="When I close the window"
    path="general.close_behavior"
    help="Minimize to tray keeps ClipCheck recording in the background."
  >
    {#snippet children(ctl)}
      <Segmented
        name={ctl.id}
        labelId={ctl.labelId}
        describedBy={ctl.describedBy}
        value={general.close_behavior}
        options={CLOSE_OPTIONS}
        {disabled}
        onchange={(v) => setPath('general.close_behavior', v)}
      />
    {/snippet}
  </FieldRow>

  <FieldRow
    label="Ask before exiting while recording"
    path="general.confirm_exit_while_recording"
    help="Show a confirmation when you exit during a session."
  >
    {#snippet children(ctl)}
      <Toggle
        {...ctl}
        checked={general.confirm_exit_while_recording}
        {disabled}
        onchange={(v) => setPath('general.confirm_exit_while_recording', v)}
      />
    {/snippet}
  </FieldRow>
{/if}
