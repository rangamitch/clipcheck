<script lang="ts">
  import { onMount } from 'svelte';
  import { encoders, loadEncoders } from '../../discovery';
  import FieldRow from '../fields/FieldRow.svelte';
  import NumberField from '../fields/NumberField.svelte';
  import Segmented from '../fields/Segmented.svelte';
  import Select from '../fields/Select.svelte';
  import Toggle from '../fields/Toggle.svelte';
  import {
    BITRATE_MAX_KBPS,
    BITRATE_MIN_KBPS,
    FPS_CHOICES,
    RESOLUTION_CHOICES,
    bitrateChoice,
    bitrateOptions,
    codecAvailability,
    codecOptions,
    cqMax,
    cqToSlider,
    gpuOptions,
    hardwareLines,
    presetCards,
    presetPatch,
    sliderToCq,
    videoSummary,
  } from '../pure';
  import { settings, setPath, type EncoderSpeed, type RateControl, type VideoCodec } from '../store';
  import { frameCoalesced } from '../frame';
  import { uniqueGpus } from '../video';

  interface Props {
    disabled?: boolean;
  }

  let { disabled = false }: Props = $props();

  /** The quality slider writes at most once per frame while it is dragged, using the current codec. */
  const dragQualitySlider = frameCoalesced((position: number) =>
    setPath('video.quality_cq', sliderToCq(position, video?.codec ?? 'h264')),
  );

  const video = $derived($settings.draft?.video);
  const cards = presetCards();
  const ready = $derived($encoders.encoderReady);
  const probeRows = $derived($encoders.encoders);
  /** The probe can list one adapter twice. Duplicate names would be duplicate keys in the lists below. */
  const gpus = $derived(uniqueGpus($encoders.gpus));

  /** Set when the user picks "Custom…" in the bitrate list, even on a value that is a stop. */
  let bitrateCustomOpen = $state(false);

  const MODE_OPTIONS: { value: 'gpu' | 'cpu'; label: string }[] = [
    { value: 'gpu', label: 'GPU' },
    { value: 'cpu', label: 'CPU' },
  ];

  const RATE_OPTIONS: { value: RateControl; label: string }[] = [
    { value: 'cbr', label: 'Constant bitrate' },
    { value: 'vbr', label: 'Variable bitrate' },
    { value: 'quality', label: 'Constant quality' },
  ];

  const SPEED_OPTIONS: { value: EncoderSpeed; label: string }[] = [
    { value: 'fastest', label: 'Fastest' },
    { value: 'balanced', label: 'Balanced' },
    { value: 'quality', label: 'Quality' },
  ];

  const RESOLUTION_OPTIONS = RESOLUTION_CHOICES.map((r) => ({ value: r.value, label: r.label }));
  const FPS_OPTIONS = FPS_CHOICES.map((f) => ({ value: String(f), label: `${f} FPS` }));
  const BITRATE_OPTIONS = bitrateOptions();

  const CAPTURE_METHOD_OPTIONS: { value: 'standard' | 'experimental'; label: string }[] = [
    { value: 'standard', label: 'Standard Capture' },
    { value: 'experimental', label: 'Experimental Capture' },
  ];

  const gpuMode = $derived<'gpu' | 'cpu'>(video?.encoder === 'software' ? 'cpu' : 'gpu');
  const codec = $derived(video?.codec ?? 'h264');
  const cqLimit = $derived(cqMax(codec));
  const summary = $derived(video ? videoSummary(video) : '');
  const codecChoices = $derived(video ? codecOptions(video.encoder, probeRows, ready) : []);
  const savedCodecState = $derived(
    video ? codecAvailability(video.codec, video.encoder, probeRows, ready) : null,
  );
  const gpuChoices = $derived(video ? gpuOptions(gpus, video.gpu, ready) : []);
  const bitrateSelect = $derived(video ? bitrateChoice(video.bitrate_kbps, bitrateCustomOpen) : 'custom');
  const hardware = $derived(hardwareLines(gpus, probeRows));

  onMount(() => {
    void loadEncoders();
  });

  /** GPU writes the encoder as "auto" (the recorder picks the vendor encoder). CPU is "software". */
  function chooseMode(mode: 'gpu' | 'cpu'): void {
    if (mode === 'cpu') {
      setPath('video.encoder', 'software');
      // AV1 has no software encoder, so switching to CPU moves AV1 to H.264.
      if (video?.codec === 'av1') setPath('video.codec', 'h264');
    } else if (video?.encoder === 'software') {
      setPath('video.encoder', 'auto');
    }
  }

  /** Changing codec also clamps quality_cq to the new codec's range. */
  function chooseCodec(next: string): void {
    const c = next as VideoCodec;
    setPath('video.codec', c);
    if (video && video.quality_cq > cqMax(c)) setPath('video.quality_cq', cqMax(c));
  }

  /** A card click writes the preset and, for a named preset, its resolution, fps and bitrate. */
  function choosePreset(value: string): void {
    for (const [key, v] of Object.entries(presetPatch(value))) setPath(`video.${key}`, v);
  }

  function chooseBitrate(value: string): void {
    if (value === 'custom') {
      bitrateCustomOpen = true;
      return;
    }
    bitrateCustomOpen = false;
    setPath('video.bitrate_kbps', Number(value));
  }
</script>

{#if video}
  <h2>Video</h2>

  <fieldset class="presets" {disabled}>
    <legend>Quality preset</legend>
    <div class="cards">
      {#each cards as card (card.value)}
        <label class="card" class:selected={video.preset === card.value}>
          <input
            type="radio"
            name="video-preset"
            value={card.value}
            checked={video.preset === card.value}
            onchange={() => choosePreset(card.value)}
          />
          <span class="card-head">
            <span class="card-name">{card.name}</span>
            {#if video.preset === card.value}
              <span class="check" aria-hidden="true">✓</span>
            {/if}
          </span>
          <span class="card-blurb">{card.blurb}</span>
          {#if card.detail}
            <span class="card-detail">{card.detail}</span>
          {/if}
        </label>
      {/each}
    </div>
  </fieldset>

  <p class="summary" aria-live="polite">{summary}</p>

  {#if video.preset === 'custom'}
    <section class="custom" aria-label="Custom settings">
      <FieldRow label="Resolution" path="video.resolution" help="Frames are scaled down to this height. Never scaled up.">
        {#snippet children(ctl)}
          <Select
            {...ctl}
            value={video.resolution}
            options={RESOLUTION_OPTIONS}
            {disabled}
            onchange={(v) => setPath('video.resolution', v)}
          />
        {/snippet}
      </FieldRow>

      <FieldRow label="Frame rate" path="video.fps" help="Frames per second.">
        {#snippet children(ctl)}
          <Select
            {...ctl}
            value={String(video.fps)}
            options={FPS_OPTIONS}
            {disabled}
            onchange={(v) => setPath('video.fps', Number(v))}
          />
        {/snippet}
      </FieldRow>

      <FieldRow
        label="Rate control"
        path="video.rate_control"
        help="Constant bitrate holds one bitrate. Variable bitrate lets busy scenes spike up to a peak. Constant quality keeps the same look and lets the file size vary."
      >
        {#snippet children(ctl)}
          <Segmented
            {...ctl}
            name="video-rate-control"
            value={video.rate_control}
            options={RATE_OPTIONS}
            {disabled}
            onchange={(v) => setPath('video.rate_control', v)}
          />
        {/snippet}
      </FieldRow>

      {#if video.rate_control === 'quality'}
        <FieldRow
          label="Image quality"
          path="video.quality_cq"
          help={`Drag right for a sharper image and a larger file. Stored as CQ ${cqLimit === 63 ? '0 to 63 (AV1)' : '0 to 51'}, where lower CQ is better.`}
        >
          {#snippet children(ctl)}
            <div class="quality">
              <span class="end">Smaller file</span>
              <input
                type="range"
                id={ctl.id}
                aria-labelledby={ctl.labelId}
                aria-describedby={ctl.describedBy}
                min="0"
                max={cqLimit}
                step="1"
                value={cqToSlider(video.quality_cq, codec)}
                {disabled}
                oninput={(e) => dragQualitySlider(Number(e.currentTarget.value))}
              />
              <span class="end">Better quality</span>
              <span class="cq-value">CQ {video.quality_cq}</span>
            </div>
          {/snippet}
        </FieldRow>
      {:else}
        <FieldRow
          label="Bitrate"
          path="video.bitrate_kbps"
          help="Target bitrate for constant and variable bitrate. Constant quality ignores it."
        >
          {#snippet children(ctl)}
            <div class="stack">
              <Select
                {...ctl}
                value={bitrateSelect}
                options={BITRATE_OPTIONS}
                {disabled}
                onchange={chooseBitrate}
              />
              {#if bitrateSelect === 'custom'}
                <NumberField
                  value={video.bitrate_kbps}
                  min={BITRATE_MIN_KBPS}
                  max={BITRATE_MAX_KBPS}
                  step={500}
                  unit="kbps"
                  ariaLabel="Bitrate in kbps"
                  {disabled}
                  onchange={(v) => setPath('video.bitrate_kbps', v)}
                />
              {/if}
            </div>
          {/snippet}
        </FieldRow>

        {#if video.rate_control === 'vbr'}
          <FieldRow
            label="Peak bitrate"
            path="video.max_bitrate_kbps"
            help="The highest bitrate variable bitrate may reach. Must be at least the target."
          >
            {#snippet children(ctl)}
              <NumberField
                {...ctl}
                value={video.max_bitrate_kbps}
                min={BITRATE_MIN_KBPS}
                max={BITRATE_MAX_KBPS}
                step={500}
                unit="kbps"
                {disabled}
                onchange={(v) => setPath('video.max_bitrate_kbps', v)}
              />
            {/snippet}
          </FieldRow>
        {/if}
      {/if}
    </section>
  {/if}

  <FieldRow
    label="Video encoder"
    path="video.encoder"
    help="GPU uses the hardware encoder on your graphics card. CPU uses the software encoder: it works everywhere but costs more processor time. AV1 is not available on CPU."
  >
    {#snippet children(ctl)}
      <Segmented
        {...ctl}
        name="video-encoder-mode"
        value={gpuMode}
        options={MODE_OPTIONS}
        {disabled}
        onchange={chooseMode}
      />
    {/snippet}
  </FieldRow>

  {#if gpuMode === 'gpu'}
    <FieldRow
      label="Selected GPU"
      path="video.gpu"
      help="The graphics card that encodes your recordings. Automatic picks the best one on this PC."
    >
      {#snippet children(ctl)}
        <Select
          {...ctl}
          value={video.gpu}
          options={gpuChoices}
          {disabled}
          onchange={(v) => setPath('video.gpu', v)}
        />
      {/snippet}
    </FieldRow>
  {/if}

  <FieldRow label="Codec" path="video.codec" help="H.264 plays everywhere. H.265 and AV1 make smaller files, but not every PC can encode them.">
    {#snippet children(ctl)}
      <Select
        {...ctl}
        value={video.codec}
        options={codecChoices}
        {disabled}
        onchange={chooseCodec}
      />
      {#if !ready}
        <p class="note" role="status">Checking your hardware…</p>
      {:else if savedCodecState && !savedCodecState.available}
        <p class="warn" role="note">{savedCodecState.reason} Pick another codec or encoder.</p>
      {/if}
    {/snippet}
  </FieldRow>

  <details class="advanced">
    <summary>Advanced</summary>

    <FieldRow
      label="Encoder speed"
      path="video.encoder_speed"
      help="Fastest uses the least GPU or CPU time, for a larger file at the same quality. Quality uses more encoder work for a smaller file that looks the same. Balanced is the default. Slower speeds raise GPU load while recording."
    >
      {#snippet children(ctl)}
        <Segmented
          {...ctl}
          name="video-encoder-speed"
          value={video.encoder_speed}
          options={SPEED_OPTIONS}
          {disabled}
          onchange={(v) => setPath('video.encoder_speed', v)}
        />
      {/snippet}
    </FieldRow>

    <FieldRow
      label="B-frames"
      path="video.b_frames"
      help="Frames predicted from both the past and the future. More B-frames make smaller files and add a little encoding delay. 0 to 4."
    >
      {#snippet children(ctl)}
        <NumberField
          {...ctl}
          value={video.b_frames}
          min={0}
          max={4}
          step={1}
          {disabled}
          onchange={(v) => setPath('video.b_frames', v)}
        />
      {/snippet}
    </FieldRow>

    <FieldRow
      label="Keyframe interval"
      path="video.keyframe_interval_s"
      help="Seconds between keyframes. Also the length of each replay segment. 1 to 4."
    >
      {#snippet children(ctl)}
        <NumberField
          {...ctl}
          value={video.keyframe_interval_s}
          min={1}
          max={4}
          step={1}
          unit="seconds"
          {disabled}
          onchange={(v) => setPath('video.keyframe_interval_s', v)}
        />
      {/snippet}
    </FieldRow>

    <FieldRow label="Show the mouse cursor" path="video.cursor" help="Draws the cursor in recorded frames.">
      {#snippet children(ctl)}
        <Toggle
          {...ctl}
          checked={video.cursor}
          {disabled}
          onchange={(v) => setPath('video.cursor', v)}
        />
      {/snippet}
    </FieldRow>

    <FieldRow
      label="Capture Method"
      path="video.no_border_capture"
      help="Standard Capture records the game window and is right for most people. Experimental Capture records the screen area under the game instead, so Windows never draws its coloured outline. It can fix border and recording problems on Windows 10 and is also usable on Windows 11. A notification or window on top of the game will appear in your clips. It is only used when the game is borderless fullscreen; otherwise Standard Capture is used."
    >
      {#snippet children(ctl)}
        <Segmented
          {...ctl}
          name="video-capture-method"
          value={video.no_border_capture ? 'experimental' : 'standard'}
          options={CAPTURE_METHOD_OPTIONS}
          {disabled}
          onchange={(v) => setPath('video.no_border_capture', v === 'experimental')}
        />
      {/snippet}
    </FieldRow>
  </details>

  <details class="hardware">
    <summary>Your hardware</summary>
    {#if !ready}
      <p class="note">Checking your hardware…</p>
    {:else}
      {#each hardware as line, i (i)}
        <p class="line">{line}</p>
      {:else}
        <p class="line">No graphics card or encoder was found.</p>
      {/each}
    {/if}
  </details>
{/if}

<style>
  fieldset {
    border: 0;
    margin: 0;
    padding: 14px 0 6px;
    min-width: 0;
  }

  legend {
    padding: 0;
    margin-bottom: 10px;
    font-weight: 600;
    color: var(--text);
  }

  fieldset:disabled {
    opacity: 0.6;
  }

  .cards {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(168px, 1fr));
    gap: 10px;
  }

  .card {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 12px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    background: var(--surface-2);
    cursor: pointer;
    min-width: 0;
  }

  .card.selected {
    border-color: var(--accent);
    box-shadow: inset 0 0 0 1px var(--accent);
    background: var(--surface);
  }

  .card:has(input:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }

  .card input {
    position: absolute;
    opacity: 0;
    width: 1px;
    height: 1px;
    margin: 0;
    pointer-events: none;
  }

  .card-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
  }

  .card-name {
    font-weight: 600;
    color: var(--text);
  }

  .check {
    color: var(--accent);
    font-weight: 700;
  }

  .card-blurb {
    font-size: 13px;
    color: var(--text-dim);
  }

  .card-detail {
    font-size: 13px;
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }

  .summary {
    margin: 12px 0 0;
    font-size: 13px;
    color: var(--text);
  }

  .custom {
    margin-top: 6px;
  }

  .quality {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    max-width: 480px;
  }

  .quality input[type='range'] {
    flex: 1 1 160px;
    min-width: 120px;
    accent-color: var(--accent);
  }

  .end {
    font-size: 12px;
    color: var(--text-dim);
  }

  .cq-value {
    min-width: 56px;
    font-variant-numeric: tabular-nums;
    color: var(--text);
  }

  .stack {
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: flex-start;
  }

  .note {
    margin: 0;
    font-size: 13px;
    color: var(--text-dim);
  }

  .warn {
    margin: 0;
    font-size: 13px;
    color: var(--danger);
  }

  .advanced,
  .hardware {
    margin-top: 18px;
  }

  .advanced > summary,
  .hardware > summary {
    cursor: pointer;
    color: var(--text-dim);
    font-weight: 600;
    padding: 6px 0;
  }

  .advanced > summary:focus-visible,
  .hardware > summary:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    border-radius: 4px;
  }

  .line {
    margin: 4px 0;
    font-size: 13px;
    color: var(--text);
  }
</style>
