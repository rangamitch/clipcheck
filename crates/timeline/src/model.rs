//! The editor's data model (docs/design/13-video-editor.md section 4). Plain data, no behaviour
//! beyond small derived getters. All edits go through the operations in `ops_*` and `cmd`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::export::ExportSettings;
use crate::time::{Rate, Tick, TICKS_PER_SECOND};

/// Current project schema version.
pub const SCHEMA_VERSION: u32 = 1;

/// New unique id (ULID string).
pub fn new_id() -> String {
    ulid::Ulid::new().to_string()
}

/// RGBA colour, 8 bits per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgba {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha (255 = opaque).
    pub a: u8,
}

impl Rgba {
    /// Opaque black.
    pub const BLACK: Rgba = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    /// Opaque white.
    pub const WHITE: Rgba = Rgba {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
}

/// Output canvas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Canvas {
    /// Width in pixels (even, 128..=8192).
    pub width: u32,
    /// Height in pixels (even, 128..=8192).
    pub height: u32,
    /// Project frame rate.
    pub fps: Rate,
    /// Colour shown where nothing covers the canvas.
    pub background: Rgba,
    /// Preset id such as "16:9", "9:16", "1:1", "custom". Informational.
    pub aspect_preset: String,
    /// When true, fit-mode clips get a blurred, scaled copy of themselves behind them.
    pub blur_background: bool,
}

impl Default for Canvas {
    fn default() -> Self {
        Canvas {
            width: 1920,
            height: 1080,
            fps: Rate::whole(30),
            background: Rgba::BLACK,
            aspect_preset: "16:9".to_string(),
            blur_background: false,
        }
    }
}

/// Kind of a media file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// Video (with or without audio).
    Video,
    /// Audio only.
    Audio,
    /// Still image.
    Image,
}

/// One audio stream of a media file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaAudio {
    /// Display title ("Mix", "Game", "Microphone", ...). Empty when unknown.
    pub title: String,
    /// Codec name ("aac", "mp3", ...).
    pub codec: String,
    /// Channel count.
    pub channels: u32,
    /// Sample rate in Hz.
    pub sample_rate: u32,
}

impl Default for MediaAudio {
    fn default() -> Self {
        MediaAudio {
            title: String::new(),
            codec: String::new(),
            channels: 2,
            sample_rate: 48_000,
        }
    }
}

/// Identity of a file on disk, used to notice moved or changed media.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Fingerprint {
    /// Size in bytes.
    pub size: u64,
    /// Modified time, ms since the Unix epoch.
    pub mtime_ms: i64,
}

/// A file in the project's media bin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaRef {
    /// Unique id.
    pub id: String,
    /// What it is.
    pub kind: MediaKind,
    /// Display name (usually the file name).
    pub name: String,
    /// Absolute path.
    pub path: String,
    /// Library recording id when it came from the ClipCheck gallery.
    pub library_recording_id: Option<String>,
    /// File identity at import time.
    pub fingerprint: Fingerprint,
    /// Duration of the file (zero for images).
    pub duration: Tick,
    /// Video width (0 without video).
    pub width: u32,
    /// Video height (0 without video).
    pub height: u32,
    /// Video frame rate (0 without video).
    pub fps: f32,
    /// Video codec name ("h264", "hevc", ...), empty without video.
    pub video_codec: String,
    /// Audio streams in file order.
    pub audio: Vec<MediaAudio>,
    /// True when the file is not found or no longer matches its fingerprint.
    pub missing: bool,
}

impl Default for MediaRef {
    fn default() -> Self {
        MediaRef {
            id: String::new(),
            kind: MediaKind::Video,
            name: String::new(),
            path: String::new(),
            library_recording_id: None,
            fingerprint: Fingerprint::default(),
            duration: Tick::ZERO,
            width: 0,
            height: 0,
            fps: 0.0,
            video_codec: String::new(),
            audio: Vec::new(),
            missing: false,
        }
    }
}

/// Track kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    /// Holds video, image, title and solid clips.
    Video,
    /// Holds audio clips.
    Audio,
}

/// A timeline track. Video tracks stack bottom to top in `Project::tracks` order (the first video
/// track is the bottom layer). Audio tracks are listed top to bottom. The two kinds are ordered
/// independently of each other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Track {
    /// Unique id.
    pub id: String,
    /// Video or audio.
    pub kind: TrackKind,
    /// Display name ("V1", "A1", ...).
    pub name: String,
    /// Muted (audio silent, video not rendered).
    pub muted: bool,
    /// Solo: when any track of the kind is solo, only solo tracks play.
    pub solo: bool,
    /// Locked tracks refuse edits.
    pub locked: bool,
    /// Hidden video tracks are not rendered.
    pub hidden: bool,
    /// Track fader in dB (audio tracks), -60..=12.
    pub volume_db: f64,
    /// Receives Insert/Overwrite/Split-all (track targeting, F-TL-23).
    pub targeted: bool,
    /// Row height in px (cosmetic).
    pub height: u32,
}

impl Default for Track {
    fn default() -> Self {
        Track {
            id: String::new(),
            kind: TrackKind::Video,
            name: String::new(),
            muted: false,
            solo: false,
            locked: false,
            hidden: false,
            volume_db: 0.0,
            targeted: true,
            height: 56,
        }
    }
}

/// What a clip is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipKind {
    /// Video from a media file.
    Video,
    /// One audio stream of a media file (or an audio file).
    Audio,
    /// Still image.
    Image,
    /// Text title.
    Title,
    /// Solid colour generator.
    Solid,
}

/// How a clip is fitted to the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitMode {
    /// Whole frame visible, bars where aspect differs.
    Fit,
    /// Canvas filled, edges cropped.
    Fill,
    /// Stretched to the canvas ignoring aspect.
    Stretch,
    /// Fit, then `scale`, `pos_x`, `pos_y` apply.
    Custom,
}

/// Crop as fractions (0..0.9) of the source frame removed from each side.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Crop {
    /// Left.
    pub left: f64,
    /// Top.
    pub top: f64,
    /// Right.
    pub right: f64,
    /// Bottom.
    pub bottom: f64,
}

/// Geometry of a video, image or title clip.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transform {
    /// Fit mode.
    pub fit: FitMode,
    /// Extra scale factor (1.0 = fitted size), 0.05..=8.
    pub scale: f64,
    /// Horizontal offset from centre, as a fraction of canvas width (-2..=2).
    pub pos_x: f64,
    /// Vertical offset from centre, as a fraction of canvas height (-2..=2).
    pub pos_y: f64,
    /// Rotation in quarter turns clockwise (0..=3).
    pub rotate_90: u8,
    /// Mirror left-right.
    pub flip_h: bool,
    /// Mirror top-bottom.
    pub flip_v: bool,
    /// Crop.
    pub crop: Crop,
}

impl Default for Transform {
    fn default() -> Self {
        Transform {
            fit: FitMode::Fit,
            scale: 1.0,
            pos_x: 0.0,
            pos_y: 0.0,
            rotate_90: 0,
            flip_h: false,
            flip_v: false,
            crop: Crop::default(),
        }
    }
}

/// Fade curve shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FadeCurve {
    /// Straight line in amplitude.
    Linear,
    /// Equal power (sine/cosine), for crossfades.
    EqualPower,
}

/// One point of a clip's volume envelope.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EnvPoint {
    /// Time from the clip's start on the timeline.
    pub t: Tick,
    /// Gain in dB at that time.
    pub db: f64,
}

/// Audio properties of a clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipAudio {
    /// Which audio stream of the media file this clip plays (0-based).
    pub source_track: u32,
    /// Clip gain in dB, -60..=12.
    pub gain_db: f64,
    /// Muted.
    pub muted: bool,
    /// Pan -1.0 (left) ..= 1.0 (right).
    pub pan: f64,
    /// Fade-in length.
    pub fade_in: Tick,
    /// Fade-out length.
    pub fade_out: Tick,
    /// Fade shape.
    pub fade_curve: FadeCurve,
    /// Volume envelope points, sorted by `t`.
    pub envelope: Vec<EnvPoint>,
    /// Mix a stereo source down to mono (both channels equal).
    pub mono: bool,
}

impl Default for ClipAudio {
    fn default() -> Self {
        ClipAudio {
            source_track: 0,
            gain_db: 0.0,
            muted: false,
            pan: 0.0,
            fade_in: Tick::ZERO,
            fade_out: Tick::ZERO,
            fade_curve: FadeCurve::Linear,
            envelope: Vec::new(),
            mono: false,
        }
    }
}

/// Transition at the start of a clip, blending from the previous clip on the same track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    /// Cross-dissolve (audio equal-power crossfade).
    Crossfade,
    /// Fade to black then up.
    DipToBlack,
    /// Fade to white then up.
    DipToWhite,
}

/// A transition into a clip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    /// Kind.
    pub kind: TransitionKind,
    /// Length (0.1 s..=5 s).
    pub duration: Tick,
}

/// Text title properties.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextSpec {
    /// The text (may contain newlines).
    pub text: String,
    /// Font family name (system font).
    pub font: String,
    /// Font size in pixels relative to a 1080-px-high canvas.
    pub size: f64,
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Text colour.
    pub color: Rgba,
    /// Outline colour, if any.
    pub outline: Option<Rgba>,
    /// Background box colour, if any.
    pub box_bg: Option<Rgba>,
    /// "left", "center" or "right".
    pub align: String,
}

impl Default for TextSpec {
    fn default() -> Self {
        TextSpec {
            text: "Title".to_string(),
            font: "Segoe UI".to_string(),
            size: 64.0,
            bold: true,
            italic: false,
            color: Rgba::WHITE,
            outline: Some(Rgba::BLACK),
            box_bg: None,
            align: "center".to_string(),
        }
    }
}

/// A clip on a track.
///
/// `dur` is the length on the timeline. The length in the source is `dur * speed`
/// (see [`Clip::src_len`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Clip {
    /// Unique id.
    pub id: String,
    /// Track holding the clip.
    pub track_id: String,
    /// What it is.
    pub kind: ClipKind,
    /// Media file (None for titles and solids).
    pub media_id: Option<String>,
    /// Display name.
    pub name: String,
    /// Start on the timeline.
    pub start: Tick,
    /// Length on the timeline.
    pub dur: Tick,
    /// Start inside the source file (source time, before speed).
    pub src_in: Tick,
    /// Playback speed, 0.1..=8.0 (1.0 = normal).
    pub speed: f64,
    /// Play backwards.
    pub reverse: bool,
    /// Disabled clips are skipped by preview and export but stay on the timeline.
    pub enabled: bool,
    /// Label colour (CSS hex), cosmetic.
    pub color: Option<String>,
    /// Geometry (video, image, title).
    pub transform: Transform,
    /// Opacity 0..=1 (video, image, title).
    pub opacity: f64,
    /// Audio properties (audio clips).
    pub audio: ClipAudio,
    /// Soft link shared by an audio/video pair (F-GRP-5).
    pub link_id: Option<String>,
    /// User group (F-GRP-1).
    pub group_id: Option<String>,
    /// Transition into this clip.
    pub transition_in: Option<Transition>,
    /// Title text (kind Title).
    pub text: Option<TextSpec>,
    /// Fill colour (kind Solid).
    pub solid: Option<Rgba>,
}

impl Default for Clip {
    fn default() -> Self {
        Clip {
            id: String::new(),
            track_id: String::new(),
            kind: ClipKind::Video,
            media_id: None,
            name: String::new(),
            start: Tick::ZERO,
            dur: Tick::ZERO,
            src_in: Tick::ZERO,
            speed: 1.0,
            reverse: false,
            enabled: true,
            color: None,
            transform: Transform::default(),
            opacity: 1.0,
            audio: ClipAudio::default(),
            link_id: None,
            group_id: None,
            transition_in: None,
            text: None,
            solid: None,
        }
    }
}

impl Clip {
    /// End on the timeline (`start + dur`).
    pub fn end(&self) -> Tick {
        self.start + self.dur
    }

    /// Length consumed from the source: `dur * speed`, rounded to the nearest tick.
    pub fn src_len(&self) -> Tick {
        let v = (self.dur.0 as f64 * self.speed).round();
        Tick(v.clamp(0.0, crate::time::MAX_TICK as f64) as i64)
    }

    /// End inside the source file (`src_in + src_len`).
    pub fn src_out(&self) -> Tick {
        self.src_in + self.src_len()
    }

    /// Timeline duration for a source length at a speed (inverse of [`Clip::src_len`]).
    pub fn dur_for_src(src_len: Tick, speed: f64) -> Tick {
        if !speed.is_finite() || speed <= 0.0 {
            return Tick::ZERO;
        }
        let v = (src_len.0 as f64 / speed).round();
        Tick(v.clamp(0.0, crate::time::MAX_TICK as f64) as i64)
    }
}

/// A user group (members reference it through `Clip::group_id`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Group {
    /// Unique id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Colour (CSS hex).
    pub color: Option<String>,
}

impl Default for Group {
    fn default() -> Self {
        Group {
            id: String::new(),
            name: "Group".to_string(),
            color: None,
        }
    }
}

/// A timeline marker.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Marker {
    /// Unique id.
    pub id: String,
    /// Position.
    pub time: Tick,
    /// Label.
    pub label: String,
    /// Colour (CSS hex).
    pub color: Option<String>,
}

/// In/out range ("work area") used for export and looping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkArea {
    /// In point.
    pub start: Tick,
    /// Out point (exclusive).
    pub end: Tick,
}

/// Cosmetic editor state restored on open.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiState {
    /// Timeline zoom in pixels per second.
    pub zoom_px_per_sec: f64,
    /// Horizontal scroll in pixels.
    pub scroll_x: f64,
    /// Playhead position.
    pub playhead: Tick,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            zoom_px_per_sec: 80.0,
            scroll_x: 0.0,
            playhead: Tick::ZERO,
        }
    }
}

/// Editing behaviour toggles saved with the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditModes {
    /// Magnetic / ripple mode: moving or deleting closes gaps on the affected tracks.
    pub magnetic: bool,
    /// Snapping on.
    pub snap: bool,
}

impl Default for EditModes {
    fn default() -> Self {
        EditModes {
            magnetic: true,
            snap: true,
        }
    }
}

/// A whole editor project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Project {
    /// Schema version (see [`SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Unique id (also the file stem of the `.ccproj`).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Created, ms since the Unix epoch.
    pub created_ms: i64,
    /// Last modified, ms since the Unix epoch.
    pub modified_ms: i64,
    /// Output canvas.
    pub canvas: Canvas,
    /// Media bin.
    pub media: Vec<MediaRef>,
    /// Tracks.
    pub tracks: Vec<Track>,
    /// All clips of all tracks.
    pub clips: Vec<Clip>,
    /// Groups.
    pub groups: Vec<Group>,
    /// Markers.
    pub markers: Vec<Marker>,
    /// In/out range.
    pub work_area: Option<WorkArea>,
    /// Editing toggles.
    pub modes: EditModes,
    /// Export settings last used with this project.
    pub export_defaults: ExportSettings,
    /// Cosmetic UI state.
    pub ui_state: UiState,
    /// Fields written by newer versions, kept so saving does not lose them.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for Project {
    fn default() -> Self {
        Project {
            schema_version: SCHEMA_VERSION,
            id: String::new(),
            name: "Untitled".to_string(),
            created_ms: 0,
            modified_ms: 0,
            canvas: Canvas::default(),
            media: Vec::new(),
            tracks: Vec::new(),
            clips: Vec::new(),
            groups: Vec::new(),
            markers: Vec::new(),
            work_area: None,
            modes: EditModes::default(),
            export_defaults: ExportSettings::default(),
            ui_state: UiState::default(),
            extra: BTreeMap::new(),
        }
    }
}

impl Project {
    /// A new empty project with default tracks V1..V3 and A1..A4 and a fresh id.
    pub fn new(name: &str) -> Project {
        let mut p = Project {
            id: new_id(),
            name: name.to_string(),
            ..Project::default()
        };
        for i in 1..=3 {
            p.tracks.push(Track {
                id: new_id(),
                kind: TrackKind::Video,
                name: format!("V{i}"),
                ..Track::default()
            });
        }
        for i in 1..=4 {
            p.tracks.push(Track {
                id: new_id(),
                kind: TrackKind::Audio,
                name: format!("A{i}"),
                ..Track::default()
            });
        }
        p
    }

    /// Clip by id.
    pub fn clip(&self, id: &str) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id == id)
    }

    /// Mutable clip by id.
    pub fn clip_mut(&mut self, id: &str) -> Option<&mut Clip> {
        self.clips.iter_mut().find(|c| c.id == id)
    }

    /// Track by id.
    pub fn track(&self, id: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    /// Mutable track by id.
    pub fn track_mut(&mut self, id: &str) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    /// Media by id.
    pub fn media_ref(&self, id: &str) -> Option<&MediaRef> {
        self.media.iter().find(|m| m.id == id)
    }

    /// End of the last clip (zero for an empty timeline).
    pub fn timeline_end(&self) -> Tick {
        self.clips.iter().map(Clip::end).max().unwrap_or(Tick::ZERO)
    }

    /// Clips on one track sorted by start time.
    pub fn clips_on_track(&self, track_id: &str) -> Vec<&Clip> {
        let mut v: Vec<&Clip> = self
            .clips
            .iter()
            .filter(|c| c.track_id == track_id)
            .collect();
        v.sort_by_key(|c| c.start);
        v
    }

    /// Total timeline length in seconds as a float (display only).
    pub fn duration_secs(&self) -> f64 {
        self.timeline_end().0 as f64 / TICKS_PER_SECOND as f64
    }
}
