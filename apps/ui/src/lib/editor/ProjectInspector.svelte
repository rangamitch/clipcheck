<script lang="ts">
  // Project settings: name, canvas, frame rate, background, editing modes, markers and a summary.
  import { dispatch, project, seek, timelineEndTick } from './store';
  import { formatDuration, formatTimecode } from './ticks';
  import { clamp, parseDimension, roundEven } from './fields';
  import type { Canvas, Rate, Rgba } from './types';

  /** Canvas size presets (F-VID-1). `custom` keeps the current size. */
  interface CanvasPreset {
    id: string;
    label: string;
    aspect: string;
    width: number;
    height: number;
  }

  const CANVAS_PRESETS: CanvasPreset[] = [
    { id: '16:9-1920x1080', label: '16:9, 1920 x 1080', aspect: '16:9', width: 1920, height: 1080 },
    { id: '16:9-2560x1440', label: '16:9, 2560 x 1440', aspect: '16:9', width: 2560, height: 1440 },
    { id: '16:9-3840x2160', label: '16:9, 3840 x 2160', aspect: '16:9', width: 3840, height: 2160 },
    { id: '16:9-1280x720', label: '16:9, 1280 x 720', aspect: '16:9', width: 1280, height: 720 },
    { id: '9:16-1080x1920', label: '9:16, 1080 x 1920', aspect: '9:16', width: 1080, height: 1920 },
    { id: '1:1-1080x1080', label: '1:1, 1080 x 1080', aspect: '1:1', width: 1080, height: 1080 },
    { id: '4:5-1080x1350', label: '4:5, 1080 x 1350', aspect: '4:5', width: 1080, height: 1350 },
    { id: '21:9-2560x1080', label: '21:9, 2560 x 1080', aspect: '21:9', width: 2560, height: 1080 },
    { id: '4:3-1440x1080', label: '4:3, 1440 x 1080', aspect: '4:3', width: 1440, height: 1080 },
  ];
  const CUSTOM = 'custom';
  const MIN_DIM = 128;
  const MAX_DIM = 8192;
  const MAX_NAME = 120;
  const MAX_MARKER_LABEL = 80;

  /** Frame rates offered in the dropdown. */
  const FPS_OPTIONS: { key: string; label: string; rate: Rate }[] = [
    { key: '24000/1001', label: '23.976', rate: { num: 24000, den: 1001 } },
    { key: '24/1', label: '24', rate: { num: 24, den: 1 } },
    { key: '25/1', label: '25', rate: { num: 25, den: 1 } },
    { key: '30000/1001', label: '29.97', rate: { num: 30000, den: 1001 } },
    { key: '30/1', label: '30', rate: { num: 30, den: 1 } },
    { key: '50/1', label: '50', rate: { num: 50, den: 1 } },
    { key: '60000/1001', label: '59.94', rate: { num: 60000, den: 1001 } },
    { key: '60/1', label: '60', rate: { num: 60, den: 1 } },
    { key: '120/1', label: '120', rate: { num: 120, den: 1 } },
  ];

  const p = $derived($project);

  /** Preset id that matches the canvas size, or `custom`. */
  function presetFor(w: number, h: number): CanvasPreset | null {
    return CANVAS_PRESETS.find((x) => x.width === w && x.height === h) ?? null;
  }

  /** Aspect id stored in the canvas for a size (`custom` when no preset matches). */
  function aspectFor(w: number, h: number): string {
    return presetFor(w, h)?.aspect ?? CUSTOM;
  }

  /** Selected dropdown value: the stored aspect decides whether the user chose Custom. */
  const presetKey = $derived.by((): string => {
    if (!p) return CUSTOM;
    if (p.canvas.aspect_preset === CUSTOM) return CUSTOM;
    return presetFor(p.canvas.width, p.canvas.height)?.id ?? CUSTOM;
  });

  /** Dropdown value for the frame rate, as `num/den`. */
  const fpsKey = $derived(p ? `${p.canvas.fps.num}/${p.canvas.fps.den}` : '');
  const fpsKnown = $derived(FPS_OPTIONS.some((o) => o.key === fpsKey));
  const backgroundHex = $derived(p ? toHex(p.canvas.background) : '#000000');

  /** Typed width and height while the user edits them, or null when showing the project value. */
  let widthDraft = $state<string | null>(null);
  let heightDraft = $state<string | null>(null);
  /** Keep the height in proportion to the width when true. */
  let lockAspect = $state(false);
  /** Validation message for the size fields, or null. */
  let dimError = $state<string | null>(null);
  /** Project name while typed, or null when showing the project value. */
  let nameDraft = $state<string | null>(null);
  /** Marker label drafts by marker id. */
  let labelDrafts = $state<Record<string, string>>({});

  /** Canvas size is an even number in range. */
  function validDim(v: number): boolean {
    return Number.isInteger(v) && v % 2 === 0 && v >= MIN_DIM && v <= MAX_DIM;
  }

  /** Sends the whole canvas object. Drafts clear once the change lands. */
  function commitCanvas(next: Canvas, done?: () => void): void {
    void dispatch({ type: 'set_canvas', canvas: next }).then(() => {
      if (done) done();
    });
  }

  /** Commits the typed width (on change or Enter). Height follows when the aspect is locked. */
  function commitWidth(text: string): void {
    const c = p?.canvas;
    if (!c) return;
    const w = parseDimension(text);
    if (w === null || !validDim(w)) {
      dimError = `Width must be an even number from ${MIN_DIM} to ${MAX_DIM}.`;
      return;
    }
    dimError = null;
    const h = lockAspect ? clamp(roundEven((w * c.height) / c.width), MIN_DIM, MAX_DIM) : c.height;
    if (w === c.width && h === c.height) {
      widthDraft = null;
      return;
    }
    commitCanvas(
      { ...c, width: w, height: h, aspect_preset: aspectFor(w, h) },
      () => {
        widthDraft = null;
        heightDraft = null;
      },
    );
  }

  /** Commits the typed height (on change or Enter). Width follows when the aspect is locked. */
  function commitHeight(text: string): void {
    const c = p?.canvas;
    if (!c) return;
    const h = parseDimension(text);
    if (h === null || !validDim(h)) {
      dimError = `Height must be an even number from ${MIN_DIM} to ${MAX_DIM}.`;
      return;
    }
    dimError = null;
    const w = lockAspect ? clamp(roundEven((h * c.width) / c.height), MIN_DIM, MAX_DIM) : c.width;
    if (w === c.width && h === c.height) {
      heightDraft = null;
      return;
    }
    commitCanvas(
      { ...c, width: w, height: h, aspect_preset: aspectFor(w, h) },
      () => {
        widthDraft = null;
        heightDraft = null;
      },
    );
  }

  /** Applies a preset from the dropdown. Custom only marks the aspect as custom. */
  function choosePreset(id: string): void {
    const c = p?.canvas;
    if (!c) return;
    if (id === CUSTOM) {
      commitCanvas({ ...c, aspect_preset: CUSTOM });
      return;
    }
    const preset = CANVAS_PRESETS.find((x) => x.id === id);
    if (!preset) return;
    // A preset replaces the size, so any bad width or height text is dropped with the error.
    dimError = null;
    widthDraft = null;
    heightDraft = null;
    commitCanvas({ ...c, width: preset.width, height: preset.height, aspect_preset: preset.aspect });
  }

  /** Applies a frame rate from the dropdown. */
  function chooseFps(key: string): void {
    const c = p?.canvas;
    const opt = FPS_OPTIONS.find((o) => o.key === key);
    if (!c || !opt) return;
    commitCanvas({ ...c, fps: opt.rate });
  }

  /** Sets the background colour from `#rrggbb`. Alpha stays 255. */
  function chooseBackground(hex: string): void {
    const c = p?.canvas;
    const rgb = fromHex(hex);
    if (!c || !rgb) return;
    commitCanvas({ ...c, background: { r: rgb.r, g: rgb.g, b: rgb.b, a: 255 } });
  }

  /** Turns a colour into `#rrggbb`. */
  function toHex(c: Rgba): string {
    return `#${[c.r, c.g, c.b].map((v) => clamp(Math.round(v), 0, 255).toString(16).padStart(2, '0')).join('')}`;
  }

  /** Parses `#rrggbb`, or returns null. */
  function fromHex(hex: string): { r: number; g: number; b: number } | null {
    const m = /^#([0-9a-fA-F]{6})$/.exec(hex);
    const digits = m?.[1];
    if (!digits) return null;
    const n = parseInt(digits, 16);
    return { r: (n >> 16) & 255, g: (n >> 8) & 255, b: n & 255 };
  }

  /** Commits the project name (on blur or Enter). Empty or too long names are ignored. */
  function commitName(): void {
    const draft = nameDraft;
    nameDraft = null;
    if (draft === null || !p) return;
    const next = draft.trim();
    if (next.length === 0 || next.length > MAX_NAME || next === p.name) return;
    void dispatch({ type: 'rename_project', name: next });
  }

  /** Turns one editing mode on or off, keeping the other as it is. */
  function setMode(which: 'magnetic' | 'snap', on: boolean): void {
    if (!p) return;
    const magnetic = which === 'magnetic' ? on : p.modes.magnetic;
    const snap = which === 'snap' ? on : p.modes.snap;
    void dispatch({ type: 'set_modes', magnetic, snap });
  }

  /** Commits a marker label draft. */
  function commitMarkerLabel(id: string, current: string): void {
    const draft = labelDrafts[id];
    delete labelDrafts[id];
    if (draft === undefined) return;
    const next = draft.trim();
    if (next === current || next.length > MAX_MARKER_LABEL) return;
    void dispatch({ type: 'edit_marker', marker_id: id, time: null, label: next });
  }

  /** Track counts shown in the summary. */
  const videoTracks = $derived(p ? p.tracks.filter((t) => t.kind === 'video').length : 0);
  const audioTracks = $derived(p ? p.tracks.filter((t) => t.kind === 'audio').length : 0);
</script>

{#if p}
  <div class="pi">
    <section class="block" aria-labelledby="pi-project-h">
      <h3 id="pi-project-h">Project</h3>
      <div class="row">
        <label for="pi-name">Name</label>
        <input
          id="pi-name"
          type="text"
          maxlength={MAX_NAME}
          value={nameDraft ?? p.name}
          oninput={(e) => (nameDraft = e.currentTarget.value)}
          onblur={commitName}
          onkeydown={(e) => {
            if (e.key === 'Enter') {
              e.preventDefault();
              e.currentTarget.blur();
            } else if (e.key === 'Escape') {
              nameDraft = null;
              e.currentTarget.blur();
            }
          }}
        />
      </div>
      <dl class="stats">
        <dt>Duration</dt>
        <dd>{formatDuration(timelineEndTick(p))}</dd>
        <dt>Clips</dt>
        <dd>{p.clips.length}</dd>
        <dt>Media</dt>
        <dd>{p.media.length}</dd>
        <dt>Tracks</dt>
        <dd>{videoTracks} video, {audioTracks} audio</dd>
      </dl>
    </section>

    <section class="block" aria-labelledby="pi-canvas-h">
      <h3 id="pi-canvas-h">Canvas</h3>
      <div class="row">
        <label for="pi-preset">Size preset</label>
        <select
          id="pi-preset"
          value={presetKey}
          onchange={(e) => choosePreset(e.currentTarget.value)}
        >
          {#each CANVAS_PRESETS as preset (preset.id)}
            <option value={preset.id}>{preset.label}</option>
          {/each}
          <option value={CUSTOM}>Custom</option>
        </select>
      </div>
      <div class="row dims">
        <div class="field">
          <label for="pi-width">Width</label>
          <input
            id="pi-width"
            type="number"
            inputmode="numeric"
            min={MIN_DIM}
            max={MAX_DIM}
            step={2}
            value={widthDraft ?? String(p.canvas.width)}
            oninput={(e) => (widthDraft = e.currentTarget.value)}
            onchange={(e) => commitWidth(e.currentTarget.value)}
          />
        </div>
        <div class="field">
          <label for="pi-height">Height</label>
          <input
            id="pi-height"
            type="number"
            inputmode="numeric"
            min={MIN_DIM}
            max={MAX_DIM}
            step={2}
            value={heightDraft ?? String(p.canvas.height)}
            oninput={(e) => (heightDraft = e.currentTarget.value)}
            onchange={(e) => commitHeight(e.currentTarget.value)}
          />
        </div>
      </div>
      <div class="row">
        <input id="pi-lock" type="checkbox" bind:checked={lockAspect} />
        <label for="pi-lock">Lock aspect ratio</label>
      </div>
      {#if dimError}
        <p class="error" role="alert">{dimError}</p>
      {/if}
      <p class="hint">Changing the canvas keeps every clip where it is on the timeline.</p>
    </section>

    <section class="block" aria-labelledby="pi-output-h">
      <h3 id="pi-output-h">Output</h3>
      <div class="row">
        <label for="pi-fps">Frame rate</label>
        <select id="pi-fps" value={fpsKey} onchange={(e) => chooseFps(e.currentTarget.value)}>
          {#each FPS_OPTIONS as opt (opt.key)}
            <option value={opt.key}>{opt.label} fps</option>
          {/each}
          {#if !fpsKnown}
            <option value={fpsKey}>{p.canvas.fps.num}/{p.canvas.fps.den} fps</option>
          {/if}
        </select>
      </div>
      <div class="row">
        <label for="pi-bg">Background colour</label>
        <input
          id="pi-bg"
          type="color"
          value={backgroundHex}
          onchange={(e) => chooseBackground(e.currentTarget.value)}
        />
      </div>
      <div class="row">
        <input
          id="pi-blur"
          type="checkbox"
          checked={p.canvas.blur_background}
          onchange={(e) => {
            const c = p?.canvas;
            if (c) commitCanvas({ ...c, blur_background: e.currentTarget.checked });
          }}
        />
        <label for="pi-blur">Blur background for fit clips</label>
      </div>
    </section>

    <section class="block" aria-labelledby="pi-modes-h">
      <h3 id="pi-modes-h">Editing</h3>
      <div class="row">
        <input
          id="pi-magnetic"
          type="checkbox"
          checked={p.modes.magnetic}
          onchange={(e) => setMode('magnetic', e.currentTarget.checked)}
        />
        <label for="pi-magnetic">Magnetic timeline</label>
      </div>
      <div class="row">
        <input
          id="pi-snap"
          type="checkbox"
          checked={p.modes.snap}
          onchange={(e) => setMode('snap', e.currentTarget.checked)}
        />
        <label for="pi-snap">Snap to edges and markers</label>
      </div>
    </section>

    <section class="block" aria-labelledby="pi-markers-h">
      <h3 id="pi-markers-h">Markers</h3>
      {#if p.markers.length === 0}
        <p class="hint">No markers yet.</p>
      {:else}
        <ul class="markers">
          {#each p.markers as m (m.id)}
            <li>
              <button
                type="button"
                class="tc"
                aria-label={`Go to marker at ${formatTimecode(m.time, p.canvas.fps)}`}
                onclick={() => seek(m.time)}
              >
                {formatTimecode(m.time, p.canvas.fps)}
              </button>
              <input
                type="text"
                class="label"
                aria-label={`Label for marker at ${formatTimecode(m.time, p.canvas.fps)}`}
                maxlength={MAX_MARKER_LABEL}
                value={labelDrafts[m.id] ?? m.label}
                oninput={(e) => (labelDrafts[m.id] = e.currentTarget.value)}
                onblur={() => commitMarkerLabel(m.id, m.label)}
                onkeydown={(e) => {
                  if (e.key === 'Enter') {
                    e.preventDefault();
                    e.currentTarget.blur();
                  } else if (e.key === 'Escape') {
                    delete labelDrafts[m.id];
                    e.currentTarget.blur();
                  }
                }}
              />
              <button
                type="button"
                class="icon-btn"
                aria-label={`Delete marker at ${formatTimecode(m.time, p.canvas.fps)}`}
                title="Delete marker"
                onclick={() => void dispatch({ type: 'remove_marker', marker_id: m.id })}
              >
                <svg viewBox="0 0 24 24" width="14" height="14" aria-hidden="true">
                  <path
                    d="M6 6l12 12M18 6L6 18"
                    stroke="currentColor"
                    stroke-width="2"
                    stroke-linecap="round"
                    fill="none"
                  />
                </svg>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  </div>
{:else}
  <p class="hint">No project open.</p>
{/if}

<style>
  .pi {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 12px;
    color: var(--text);
  }
  .block {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  h3 {
    margin: 0;
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    color: var(--text-dim);
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .row > label {
    flex: 0 0 auto;
    min-width: 110px;
    color: var(--text-dim);
  }
  .dims {
    gap: 12px;
  }
  .field {
    display: flex;
    flex: 1 1 0;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .field label {
    color: var(--text-dim);
  }
  input[type='text'],
  input[type='number'],
  select {
    flex: 1 1 auto;
    min-width: 0;
    padding: 6px 8px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  input[type='color'] {
    width: 48px;
    height: 28px;
    padding: 0;
    background: none;
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  input:focus-visible,
  select:focus-visible,
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .stats {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 4px 12px;
    margin: 0;
  }
  .stats dt {
    color: var(--text-dim);
  }
  .stats dd {
    margin: 0;
  }
  .hint {
    margin: 0;
    color: var(--text-dim);
    font-size: 12px;
  }
  .error {
    margin: 0;
    color: var(--danger);
    font-size: 12px;
  }
  .markers {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .markers li {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .tc {
    flex: 0 0 auto;
    padding: 4px 6px;
    font-family: ui-monospace, Consolas, monospace;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
    cursor: pointer;
  }
  .label {
    flex: 1 1 auto;
    min-width: 0;
    padding: 4px 6px;
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: 6px;
  }
  .icon-btn {
    flex: 0 0 auto;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    padding: 0;
    background: transparent;
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-dim);
    cursor: pointer;
  }
  .icon-btn:hover {
    color: var(--danger);
  }
</style>
