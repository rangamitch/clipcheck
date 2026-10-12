/**
 * TypeScript mirrors of the video editor wire types.
 *
 * Sources: crates/timeline (model.rs, cmd.rs, export.rs, plan.rs, ops_*.rs, time.rs) and
 * apps/ui/src-tauri/src/editor/*.rs. The timeline structs keep their snake_case field names.
 * The Tauri DTOs (serde rename_all camelCase) keep camelCase. Enums are snake_case strings.
 */

/** Exact integer time: 254,016,000,000 ticks per second. Serialized as a plain JSON number. */
export type Tick = number;

/** Frame rate as a rational number (`num / den`). */
export interface Rate {
  num: number;
  den: number;
}

/** RGBA colour, 8 bits per channel. */
export interface Rgba {
  r: number;
  g: number;
  b: number;
  a: number;
}

/** Output canvas. */
export interface Canvas {
  /** Width in pixels (even, 128..=8192). */
  width: number;
  /** Height in pixels (even, 128..=8192). */
  height: number;
  /** Project frame rate. */
  fps: Rate;
  /** Colour shown where nothing covers the canvas. */
  background: Rgba;
  /** Preset id such as "16:9", "9:16", "1:1", "custom". Informational. */
  aspect_preset: string;
  /** Blurred, scaled copy of fit-mode clips behind them. */
  blur_background: boolean;
}

/** Kind of a media file. */
export type MediaKind = 'video' | 'audio' | 'image';

/** Audio stream description of a media file. */
export interface MediaAudio {
  title: string;
  codec: string;
  channels: number;
  sample_rate: number;
}

/** File fingerprint used to detect changed or missing media. */
export interface Fingerprint {
  size: number;
  mtime_ms: number;
}

/** A media file in the project bin. */
export interface MediaRef {
  id: string;
  kind: MediaKind;
  name: string;
  path: string;
  library_recording_id: string | null;
  fingerprint: Fingerprint;
  /** Duration in ticks. */
  duration: Tick;
  width: number;
  height: number;
  fps: number;
  video_codec: string;
  audio: MediaAudio[];
  missing: boolean;
}

/** Track kind. Video tracks stack in list order (first = bottom). */
export type TrackKind = 'video' | 'audio';

/** A timeline track. */
export interface Track {
  id: string;
  kind: TrackKind;
  name: string;
  muted: boolean;
  solo: boolean;
  locked: boolean;
  hidden: boolean;
  volume_db: number;
  targeted: boolean;
  /** Row height in px. */
  height: number;
}

/** Clip kind. */
export type ClipKind = 'video' | 'audio' | 'image' | 'title' | 'solid';

/** How a source frame is fitted into the canvas. */
export type FitMode = 'fit' | 'fill' | 'stretch' | 'custom';

/** Crop amounts, as fractions of the source frame. */
export interface Crop {
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** Visual placement of a clip. */
export interface Transform {
  fit: FitMode;
  scale: number;
  pos_x: number;
  pos_y: number;
  /** Quarter turns, 0..=3. */
  rotate_90: number;
  flip_h: boolean;
  flip_v: boolean;
  crop: Crop;
}

/** Fade shape. */
export type FadeCurve = 'linear' | 'equal_power';

/** Volume envelope point (time relative to the clip start). */
export interface EnvPoint {
  t: Tick;
  db: number;
}

/** Audio settings of a clip. */
export interface ClipAudio {
  source_track: number;
  gain_db: number;
  muted: boolean;
  pan: number;
  fade_in: Tick;
  fade_out: Tick;
  fade_curve: FadeCurve;
  envelope: EnvPoint[];
  mono: boolean;
}

/** Transition kind. */
export type TransitionKind = 'crossfade' | 'dip_to_black' | 'dip_to_white';

/** Transition into a clip. */
export interface Transition {
  kind: TransitionKind;
  /** Length in ticks (0.1..=5 s). */
  duration: Tick;
}

/** Title text properties. */
export interface TextSpec {
  text: string;
  font: string;
  size: number;
  bold: boolean;
  italic: boolean;
  color: Rgba;
  outline: Rgba | null;
  box_bg: Rgba | null;
  /** "left", "center" or "right". */
  align: string;
}

/** A clip on a track. */
export interface Clip {
  id: string;
  track_id: string;
  kind: ClipKind;
  media_id: string | null;
  name: string;
  /** Timeline start in ticks. */
  start: Tick;
  /** Timeline length in ticks. */
  dur: Tick;
  /** Source start in ticks. */
  src_in: Tick;
  /** Speed multiplier (0.0 = freeze frame, video only). */
  speed: number;
  reverse: boolean;
  enabled: boolean;
  /** Label colour, CSS-like string, or null. */
  color: string | null;
  transform: Transform;
  opacity: number;
  audio: ClipAudio;
  link_id: string | null;
  group_id: string | null;
  transition_in: Transition | null;
  text: TextSpec | null;
  solid: Rgba | null;
}

/** A group of clips that move and edit together. */
export interface Group {
  id: string;
  name: string;
  color: string | null;
}

/** A timeline marker. */
export interface Marker {
  id: string;
  time: Tick;
  label: string;
  color: string | null;
}

/** Work area (in/out range) in ticks. */
export interface WorkArea {
  start: Tick;
  end: Tick;
}

/** Cosmetic UI state stored with the project. */
export interface UiState {
  zoom_px_per_sec: number;
  scroll_x: number;
  playhead: Tick;
}

/** Editing modes. */
export interface EditModes {
  magnetic: boolean;
  snap: boolean;
}

/** Quality setting of an export. Internally tagged by `mode`. */
export type QualityMode =
  | { mode: 'quality'; value: number }
  | { mode: 'bitrate'; kbps: number }
  | { mode: 'cbr'; kbps: number }
  | { mode: 'target_size'; mb: number };

/** Which part of the timeline is exported. Internally tagged by `kind`. */
export type ExportRange =
  | { kind: 'entire' }
  | { kind: 'work_area' }
  | { kind: 'selected_clips'; ids: string[] };

/** Video codec. */
export type VideoCodec = 'h264' | 'hevc' | 'av1';

/** Audio layout of an export. */
export type AudioLayout = 'mix' | 'separate';

/** Loudness normalisation. Internally tagged by `mode`. */
export type Loudness = { mode: 'off' } | { mode: 'lufs'; target: number };

/** Audio part of an export. */
export interface AudioExport {
  layout: AudioLayout;
  bitrate_kbps: number;
  sample_rate: number;
  loudness: Loudness;
  limiter: boolean;
}

/** What an export produces. Internally tagged by `kind`. */
export type ExportKind =
  | { kind: 'video' }
  | { kind: 'still'; format: string; quality: number; at: Tick }
  | { kind: 'audio_only' };

/** Export settings (stored in the project as `export_defaults`, and sent with each export). */
export interface ExportSettings {
  preset: string;
  name_template: string;
  /** Output folder, or null for the configured default. */
  dir: string | null;
  range: ExportRange;
  width: number;
  height: number;
  /** Output frame rate, or null to keep the project rate. */
  fps: Rate | null;
  codec: VideoCodec;
  encoder: string;
  quality: QualityMode;
  audio: AudioExport;
  kind: ExportKind;
  lossless: boolean;
}

/** Export preset (built-in or user). */
export interface Preset {
  id: string;
  label: string;
  settings: ExportSettings;
  fixed_size: boolean;
  max_mb: number | null;
  builtin: boolean;
}

/** Size estimate of an export. */
export interface SizeEstimate {
  bytes: number;
  video_kbps: number;
  audio_kbps: number;
}

/** Placement mode when a new clip meets existing clips. */
export type PlaceMode = 'overwrite' | 'insert' | 'append';

/** Which edge of a clip. */
export type Edge = 'start' | 'end';

/** One requested clip move. `new_track` null keeps the current track. */
export interface ClipMove {
  clip_id: string;
  new_start: Tick;
  new_track: string | null;
}

/**
 * An edit request sent to the backend. Discriminated on `type` (snake_case variant names);
 * the other fields mirror `timeline::cmd::Command` exactly.
 */
export type Command =
  | { type: 'add_media'; media: MediaRef }
  | { type: 'remove_media'; media_id: string }
  | { type: 'relink_media'; media_id: string; media: MediaRef }
  | {
      type: 'place_media';
      media_id: string;
      at: Tick;
      src_in: Tick;
      src_len: Tick;
      video_track: string | null;
      mode: PlaceMode;
    }
  | { type: 'move_clips'; moves: ClipMove[] }
  | { type: 'delete_clips'; ids: string[]; ripple: boolean }
  | { type: 'split_at'; time: Tick; ids: string[] }
  | { type: 'duplicate_clips'; ids: string[]; offset: Tick }
  | { type: 'paste_clips'; clips: Clip[]; at: Tick; track_id: string | null; mode: PlaceMode }
  | { type: 'copy_clips'; ids: string[] }
  | { type: 'remove_range'; start: Tick; end: Tick; track_ids: string[]; ripple: boolean }
  | { type: 'close_gap'; track_id: string; at: Tick }
  | { type: 'close_all_gaps'; track_id: string }
  | { type: 'freeze_frame'; clip_id: string; at: Tick; dur: Tick }
  | { type: 'trim_edge'; clip_id: string; edge: Edge; new_time: Tick; ripple: boolean }
  | { type: 'roll_edit'; left_id: string; right_id: string; new_cut: Tick }
  | { type: 'slip'; clip_id: string; delta: Tick }
  | { type: 'slide'; clip_id: string; delta: Tick }
  | { type: 'set_speed'; clip_id: string; speed: number; ripple: boolean }
  | { type: 'set_reverse'; clip_id: string; reverse: boolean }
  | { type: 'join_through_edit'; left_id: string; right_id: string }
  | { type: 'set_work_area'; area: WorkArea | null }
  | { type: 'group_clips'; ids: string[] }
  | { type: 'ungroup'; ids: string[] }
  | { type: 'link_clips'; ids: string[] }
  | { type: 'unlink_clips'; ids: string[] }
  | { type: 'detach_audio'; clip_id: string }
  | { type: 'edit_group'; group_id: string; name: string | null; color?: string | null }
  | { type: 'adjust_group_gain'; group_id: string; delta_db: number }
  | { type: 'patch_clips'; ids: string[]; patch: ClipPatch }
  | { type: 'set_envelope'; clip_id: string; points: EnvPoint[] }
  | { type: 'offset_gain'; ids: string[]; delta_db: number }
  | { type: 'add_track'; kind: TrackKind }
  | { type: 'remove_track'; track_id: string }
  | { type: 'patch_track'; track_id: string; patch: TrackPatch }
  | { type: 'move_track'; track_id: string; delta: number }
  | { type: 'set_canvas'; canvas: Canvas }
  | { type: 'rename_project'; name: string }
  | { type: 'set_modes'; magnetic: boolean; snap: boolean }
  | { type: 'add_marker'; time: Tick; label: string; color: string | null }
  | {
      type: 'edit_marker';
      marker_id: string;
      time: Tick | null;
      label: string | null;
      color?: string | null;
    }
  | { type: 'remove_marker'; marker_id: string }
  | { type: 'add_title'; track_id: string; at: Tick; dur: Tick; text: TextSpec }
  | { type: 'add_solid'; track_id: string; at: Tick; dur: Tick; color: Rgba };

/**
 * Partial clip update. An absent key leaves the value alone. For `color` and `transition_in`,
 * `null` clears the value and a value sets it.
 */
export interface ClipPatch {
  name?: string;
  color?: string | null;
  enabled?: boolean;
  transform?: Transform;
  opacity?: number;
  gain_db?: number;
  muted?: boolean;
  pan?: number;
  mono?: boolean;
  source_track?: number;
  fade_in?: Tick;
  fade_out?: Tick;
  fade_curve?: FadeCurve;
  transition_in?: Transition | null;
  text?: TextSpec;
  solid?: Rgba;
}

/** Partial track update. Absent keys are left alone. */
export interface TrackPatch {
  name?: string;
  muted?: boolean;
  solo?: boolean;
  locked?: boolean;
  hidden?: boolean;
  volume_db?: number;
  targeted?: boolean;
  /** Row height 24..=200. */
  height?: number;
}

/** Edits reported back after a command: ids created and removed. */
export interface Applied {
  created: string[];
  removed: string[];
}

/** The whole project plus flags and the render plan, as the UI receives it. */
export interface EditorSnapshot {
  project: Project;
  can_undo: boolean;
  can_redo: boolean;
  dirty: boolean;
  plan: RenderPlan;
  clipboard_len: number;
}

/** Reply of `editor_apply`. */
export interface ApplyReply {
  applied: Applied;
  snapshot: EditorSnapshot;
}

/** A layer of video visible in a plan segment. */
export interface VideoLayer {
  clip_id: string;
  kind: ClipKind;
  media_id: string | null;
  /** Stacking order (0 = bottom). */
  z: number;
  /** Source time in ticks at the segment start. */
  src_time: Tick;
  speed: number;
  reverse: boolean;
  transform: Transform;
  opacity: number;
  transition: Transition | null;
  clip_start: Tick;
  text: TextSpec | null;
  solid: Rgba | null;
}

/** An audio source active in a plan segment. */
export interface AudioSource {
  clip_id: string;
  track_id: string;
  track_index: number;
  media_id: string;
  source_track: number;
  src_time: Tick;
  speed: number;
  reverse: boolean;
  clip_start: Tick;
  clip_dur: Tick;
  audio: ClipAudio;
  track_db: number;
}

/** A span of the timeline with constant layers. */
export interface PlanSegment {
  start: Tick;
  end: Tick;
  video: VideoLayer[];
  audio: AudioSource[];
}

/** Flattened render plan for preview and export. */
export interface RenderPlan {
  canvas: Canvas;
  start: Tick;
  end: Tick;
  segments: PlanSegment[];
}

/** The whole editing project. */
export interface Project {
  schema_version: number;
  id: string;
  name: string;
  created_ms: number;
  modified_ms: number;
  canvas: Canvas;
  media: MediaRef[];
  tracks: Track[];
  clips: Clip[];
  groups: Group[];
  markers: Marker[];
  work_area: WorkArea | null;
  modes: EditModes;
  export_defaults: ExportSettings;
  ui_state: UiState;
}

/** Row of the Editor home screen (`ProjectSummary` in Rust). */
export interface ProjectSummary {
  id: string;
  name: string;
  modifiedMs: number;
  durationMs: number;
  clipCount: number;
  width: number;
  height: number;
  /** True when a newer autosave exists (crash recovery). */
  hasAutosave: boolean;
  /** True when the file could not be parsed. */
  broken: boolean;
}

/** Export job state. */
export type JobState = 'queued' | 'running' | 'paused' | 'done' | 'failed' | 'cancelled';

/** An export job as shown in the UI (`JobDto` in Rust). */
export interface JobDto {
  id: string;
  projectName: string;
  state: JobState;
  /** 0..=100. */
  percent: number;
  /** Running seconds (pauses excluded). */
  elapsedS: number;
  /** Estimated seconds left, or null when unknown. */
  etaS: number | null;
  /** Speed as a multiple of real time. */
  speedX: number;
  outPath: string;
  bytes: number;
  /** "user" or "recording" when paused. */
  pausedReason: string | null;
  error: string | null;
  warnings: string[];
  encoder: string | null;
  recordingId: string | null;
}

/** Dialog defaults for the open project (`ExportDefaults` in Rust). */
export interface ExportDefaults {
  dir: string;
  freeBytes: number;
  presets: Preset[];
  settings: ExportSettings;
  alwaysAskLocation: boolean;
  encoders: string[];
}

/** Size and validation result for an export (`EstimateReply` in Rust). */
export interface EstimateReply {
  estimate: SizeEstimate;
  durationS: number;
  /** Settings problems; empty when OK. */
  problems: string[];
  /** Warning chip text, or null. */
  warning: string | null;
  outPath: string;
}

/** User settings for the editor and exports (`EditorSettings` in Rust, decision D72). */
export interface EditorSettings {
  /** Autosave interval in seconds (10..=600). */
  autosaveS: number;
  magneticDefault: boolean;
  snapDefault: boolean;
  /** "match_first_clip" or a preset id ("16:9", "9:16", "1:1", "4:5"). */
  defaultCanvas: string;
  /** "auto", "full", "half" or "quarter". */
  previewQuality: string;
  /** Cache folder override ("" = default). */
  cacheDir: string;
  /** Cache size limit in GB (1..=100). */
  cacheMaxGb: number;
  /** Decoder cache budget in MB (128..=4096). */
  decodeCacheMb: number;
  defaultPreset: string;
  encoder: string;
  pauseWhileRecording: boolean;
  openFolderWhenDone: boolean;
  filenameTemplate: string;
  lowPriority: boolean;
  alwaysAskLocation: boolean;
  /** Last export folder chosen in the dialog ("" = configured default). */
  lastExportDir: string;
  protectExportsFromQuota: boolean;
  customPresets: Preset[];
}

/** One snap point for drags (`SnapPointDto` in Rust). */
export interface SnapPointDto {
  time: Tick;
  kind: 'origin' | 'playhead' | 'clip_edge' | 'marker' | 'work_area';
}

/** Result of measuring a clip's loudness (`LoudnessReport` in Rust). */
export interface LoudnessReport {
  /** Integrated LUFS, or null when silent. */
  lufs: number | null;
  peakDb: number;
}

/** Missing or changed flag for one media file (`MediaStatus` in Rust). */
export interface MediaStatus {
  mediaId: string;
  missing: boolean;
}

/** Default canvas used by new projects. */
export function defaultCanvas(): Canvas {
  return {
    width: 1920,
    height: 1080,
    fps: { num: 30, den: 1 },
    background: { r: 0, g: 0, b: 0, a: 255 },
    aspect_preset: '16:9',
    blur_background: false,
  };
}

/** Default visual transform of a clip. */
export function defaultTransform(): Transform {
  return {
    fit: 'fit',
    scale: 1,
    pos_x: 0,
    pos_y: 0,
    rotate_90: 0,
    flip_h: false,
    flip_v: false,
    crop: { left: 0, top: 0, right: 0, bottom: 0 },
  };
}

/** Default audio settings of a clip. */
export function defaultClipAudio(): ClipAudio {
  return {
    source_track: 0,
    gain_db: 0,
    muted: false,
    pan: 0,
    fade_in: 0,
    fade_out: 0,
    fade_curve: 'linear',
    envelope: [],
    mono: false,
  };
}

/** Default export settings (balanced 1080p30 H.264). */
export function defaultExportSettings(): ExportSettings {
  return {
    preset: 'balanced_1080p30',
    name_template: '{project} {yyyy-MM-dd HH-mm-ss}',
    dir: null,
    range: { kind: 'entire' },
    width: 1920,
    height: 1080,
    fps: { num: 30, den: 1 },
    codec: 'h264',
    encoder: 'auto',
    quality: { mode: 'quality', value: 70 },
    audio: {
      layout: 'mix',
      bitrate_kbps: 192,
      sample_rate: 48000,
      loudness: { mode: 'off' },
      limiter: true,
    },
    kind: { kind: 'video' },
    lossless: false,
  };
}
