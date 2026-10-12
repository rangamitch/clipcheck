//! Config validation (doc 09 section 6.4) and apply-class diffing (sections 6.2 and 6.6).
//!
//! [`validate`] is a pure check over an in-memory [`Config`]. Runtime checks (CFG-S002,
//! CFG-V011, CFG-A009/A010, CFG-H006, CFG-R004, CFG-X002) need service state and live elsewhere.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::schema::{
    expand_vars_with, normalize_game_key, AudioApp, AudioCodec, Codec, Config, DetectionMode,
    EncoderChoice, Fps, GameOverride, KeyChord, MuteMode, Preset, Press, RateControl,
    ReplayStorage, Resolution, Storage, CONFIG_VERSION, MAX_AUDIO_APPS, MAX_AUDIO_TRACKS,
    MAX_LIBRARIES, MAX_LIBRARY_LABEL_CHARS, MAX_UPDATE_ENDPOINT_CHARS, UPDATE_INTERVAL_HOURS_MAX,
    UPDATE_INTERVAL_HOURS_MIN,
};
use super::vk::{VK_DELETE, VK_F4, VK_L, VK_PRINT_SCREEN, VK_TAB};

/// Audio gain range in dB, for the game, mic, other apps and each app (CFG-A006).
const GAIN_DB_RANGE: (f32, f32) = (-60.0, 12.0);
/// Microphone noise gate threshold range in dB (CFG-A013).
const GATE_DB_RANGE: (f32, f32) = (-100.0, -20.0);
/// Largest `video.b_frames` value (CFG-V013).
const MAX_B_FRAMES: u8 = 4;

/// Severity of a [`Diagnostic`]. Errors block save and the change; warnings never do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Blocks save (UI) and the change (service).
    Error,
    /// Shown to the user, never blocks.
    Warning,
}

/// One validation finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Doc code, for example `"CFG-V001"`.
    pub code: &'static str,
    /// Error or warning.
    pub severity: Severity,
    /// Dotted key path, for example `"audio.apps[1].name"`.
    pub path: String,
    /// Human-readable message (doc 09 section 6.4 template, filled in).
    pub message: String,
}

/// How a changed key takes effect (doc 09 section 6.6).
///
/// Ordered weakest to strongest, so `max()` over a set of changes gives the strongest class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum ApplyClass {
    /// Applies on the next read. No pipeline work.
    #[default]
    Live,
    /// Applies when a session part file opens.
    NextPart,
    /// Rebuilds capture, encode or audio encode at the next keyframe; the session splits.
    PipelineReinit,
    /// Rebuilds the WASAPI graph; up to about 100 ms of audio gap on affected tracks.
    AudioReinit,
    /// Clears the replay buffer. The UI asks first.
    ReplayFlush,
    /// Takes effect after clipcheckd restarts.
    Restart,
}

/// Maximum number of enabled clip hotkeys (CFG-H005). Shared with the hotkeys crate,
/// which re-exports this constant.
pub const MAX_CLIP_HOTKEYS: usize = 8;
/// Maximum rendered file name length in characters (CFG-S004).
const MAX_RENDERED_NAME: usize = 180;
/// Characters Windows forbids in file names (CFG-S003).
const ILLEGAL_NAME_CHARS: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
/// Longest expanded `storage.export_dir` in characters (CFG-S015).
const MAX_EXPORT_DIR_CHARS: usize = 240;
/// Characters refused in an expanded `storage.export_dir` (CFG-S015). `:` is allowed for drives.
const EXPORT_DIR_ILLEGAL_CHARS: &[char] = &['*', '?', '"', '<', '>', '|'];

/// Checks `config` against the rules in doc 09 section 6.4.
///
/// Returns every finding, errors and warnings alike. An empty vector means no findings.
pub fn validate(config: &Config) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if config.config_version > CONFIG_VERSION {
        error(
            &mut out,
            "CFG-P003",
            "config_version",
            format!(
                "config_version {} is newer than this ClipCheck supports (max {CONFIG_VERSION}). \
                 The file is read-only until you update ClipCheck",
                config.config_version
            ),
        );
    }
    check_storage(config, &mut out);
    check_libraries(config, &mut out);
    check_detection(config, &mut out);
    check_video(config, &mut out);
    check_audio(config, &mut out);
    check_replay(config, &mut out);
    check_hotkeys(config, &mut out);
    check_notifications(config, &mut out);
    check_advanced(config, &mut out);
    check_updates(config, &mut out);
    check_games(config, &mut out);
    out
}

/// Strongest apply class among the keys that differ between `old` and `new`.
///
/// Keys whose class is Live are the baseline and never raise the result. Per-game and
/// per-track keys are classed by the key table in section 6.2.
pub fn apply_class(old: &Config, new: &Config) -> ApplyClass {
    use ApplyClass::{AudioReinit, NextPart, PipelineReinit, ReplayFlush, Restart};
    let mut acc = ApplyClass::Live;

    // general: every key is Live, including close_behavior and confirm_exit_while_recording.
    // Live never raises the result, so nothing is compared here.

    // storage: root, per_game_folders, filename_template are NextPart. Every other storage key,
    // including storage.libraries (the scanner reacts), is Live.
    let (os, ns) = (&old.storage, &new.storage);
    raise(&mut acc, os.root != ns.root, NextPart);
    raise(
        &mut acc,
        os.per_game_folders != ns.per_game_folders,
        NextPart,
    );
    raise(
        &mut acc,
        os.filename_template != ns.filename_template,
        NextPart,
    );

    // video: container is NextPart; every other video key (including gpu, encoder_speed and
    // b_frames) is PipelineReinit. Comparing the structs with container aligned covers them all.
    let (ov, nv) = (&old.video, &new.video);
    raise(&mut acc, ov.container != nv.container, NextPart);
    let mut ov_rest = ov.clone();
    ov_rest.container = nv.container;
    raise(&mut acc, ov_rest != *nv, PipelineReinit);

    // audio: every audio.* key is AudioReinit. Changes take effect when the next game
    // attaches; a running recording is never disrupted.
    raise(&mut acc, old.audio != new.audio, AudioReinit);

    // replay.storage flushes the buffer; the other replay keys are Live.
    raise(
        &mut acc,
        old.replay.storage != new.replay.storage,
        ReplayFlush,
    );

    // advanced: two keys need a recorder restart; the rest are Live.
    let (oadv, nadv) = (&old.advanced, &new.advanced);
    raise(&mut acc, oadv.log_dir != nadv.log_dir, Restart);
    raise(
        &mut acc,
        oadv.encoder_extra_args != nadv.encoder_extra_args,
        Restart,
    );

    // updates: every key is Live. The update checker reads it on its next tick.

    // games: folder is NextPart, any video override is PipelineReinit, others are Live.
    let ids: BTreeSet<&String> = old.games.keys().chain(new.games.keys()).collect();
    let default_override = GameOverride::default();
    for id in ids {
        let og = old.games.get(id).unwrap_or(&default_override);
        let ng = new.games.get(id).unwrap_or(&default_override);
        raise(&mut acc, og.folder != ng.folder, NextPart);
        raise(&mut acc, og.video != ng.video, PipelineReinit);
    }

    acc
}

/// Raises `acc` to `class` when `changed` is true and `class` is stronger.
fn raise(acc: &mut ApplyClass, changed: bool, class: ApplyClass) {
    if changed && class > *acc {
        *acc = class;
    }
}

fn error(out: &mut Vec<Diagnostic>, code: &'static str, path: &str, message: String) {
    out.push(Diagnostic {
        code,
        severity: Severity::Error,
        path: path.to_owned(),
        message,
    });
}

fn warning(out: &mut Vec<Diagnostic>, code: &'static str, path: &str, message: String) {
    out.push(Diagnostic {
        code,
        severity: Severity::Warning,
        path: path.to_owned(),
        message,
    });
}

/// Inclusive range check that works for integers and floats.
fn in_range<T: PartialOrd>(value: T, lo: T, hi: T) -> bool {
    (lo..=hi).contains(&value)
}

fn check_storage(config: &Config, out: &mut Vec<Diagnostic>) {
    let s = &config.storage;
    let root = s.root.to_string_lossy();
    // %VARS% are expanded at load, so only the expanded form can be checked for absoluteness.
    if root.trim().is_empty() || (!root.contains('%') && !s.root.is_absolute()) {
        error(
            out,
            "CFG-S001",
            "storage.root",
            format!("storage.root '{root}' must be an absolute path"),
        );
    }
    if s.quota_gb != 0 && !in_range(s.quota_gb, 1, 100_000) {
        error(
            out,
            "CFG-S006",
            "storage.quota_gb",
            format!(
                "storage.quota_gb = {}. Use 0 for unlimited or 1..=100000",
                s.quota_gb
            ),
        );
    }
    check_template(&s.filename_template, out);
    check_export_dir(s, out);
}

/// Checks `storage.export_dir` after `%VARS%` expansion (CFG-S015). An empty value means the
/// default `<root>\Exports` and is always valid. Otherwise the folder must be an absolute drive
/// or UNC path, at most 240 characters, free of `* ? " < > |`, and must not name an existing
/// file. The file check is the one filesystem read in this module.
fn check_export_dir(s: &Storage, out: &mut Vec<Diagnostic>) {
    const PATH: &str = "storage.export_dir";
    let raw = s.export_dir.trim();
    if raw.is_empty() {
        return;
    }
    let expanded = expand_vars_with(raw, |name| std::env::var(name).ok());
    let reason = if !is_absolute_expanded(&expanded) {
        "must be an absolute path such as D:\\Exports or \\\\server\\share\\Exports"
    } else if expanded.chars().count() > MAX_EXPORT_DIR_CHARS {
        "is longer than 240 characters"
    } else if expanded.contains(EXPORT_DIR_ILLEGAL_CHARS) {
        "contains a character Windows does not allow in folder names (* ? \" < > |)"
    } else if std::path::Path::new(&expanded).is_file() {
        "names an existing file. Choose a folder"
    } else {
        return;
    };
    error(
        out,
        "CFG-S015",
        PATH,
        format!("storage.export_dir '{expanded}' {reason}"),
    );
}

/// True for an expanded drive path (`D:\` or `D:/`) or a UNC path (`\\server\share`).
fn is_absolute_expanded(path: &str) -> bool {
    let mut chars = path.chars();
    let drive = matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(d), Some(':'), Some('\\' | '/')) if d.is_ascii_alphabetic()
    );
    drive || path.starts_with(r"\\") || path.starts_with("//")
}

/// Normalized form of a folder path for comparisons: `/` becomes `\`, surrounding spaces and
/// trailing separators are dropped, and the text is lowercased. `F:\Medal\Clips` and
/// `f:/medal/clips/` give the same key.
fn library_key(path: &str) -> String {
    let unified: String = path
        .trim()
        .chars()
        .map(|c| if c == '/' { '\\' } else { c })
        .collect();
    unified.trim_end_matches('\\').to_lowercase()
}

/// True for a drive path (`D:\` or `D:/`), a UNC path (`\\server\share`) or a path that starts
/// with a `%VAR%` reference. Relative paths return false.
fn is_absolute_path_text(path: &str) -> bool {
    let p = path.trim();
    let b = p.as_bytes();
    let drive =
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/');
    let unc = p.starts_with(r"\\") || p.starts_with("//");
    let env = p.starts_with('%') && p[1..].contains('%');
    drive || unc || env
}

/// Checks `storage.libraries` (DESIGN.md D24): path shape, uniqueness including the primary
/// root, the count and label length, and nested folders that could be indexed twice.
fn check_libraries(config: &Config, out: &mut Vec<Diagnostic>) {
    let libs = &config.storage.libraries;
    if libs.len() > MAX_LIBRARIES {
        error(
            out,
            "CFG-S012",
            "storage.libraries",
            format!(
                "storage.libraries has {} entries. The maximum is {MAX_LIBRARIES}",
                libs.len()
            ),
        );
    }

    let root_text = config.storage.root.to_string_lossy().into_owned();
    let root_key = library_key(&root_text);
    // Every non-empty library key seen so far, with its index, for duplicate detection.
    let mut seen: Vec<(String, usize)> = Vec::new();
    for (i, lib) in libs.iter().enumerate() {
        let path = format!("storage.libraries[{i}].path");
        let text = lib.path.trim();
        if text.is_empty() {
            error(
                out,
                "CFG-S009",
                &path,
                format!("storage.libraries[{i}].path is empty. Enter a folder or remove the entry"),
            );
        } else {
            if !is_absolute_path_text(text) {
                error(
                    out,
                    "CFG-S010",
                    &path,
                    format!(
                        "storage.libraries[{i}].path '{text}' must be absolute, such as D:\\Clips, \
                         \\\\server\\share\\Clips or %USERPROFILE%\\Videos"
                    ),
                );
            }
            let key = library_key(text);
            if key == root_key {
                error(
                    out,
                    "CFG-S011",
                    &path,
                    format!(
                        "storage.libraries[{i}].path '{text}' is the same folder as storage.root"
                    ),
                );
            } else if let Some((_, j)) = seen.iter().find(|(k, _)| *k == key) {
                error(
                    out,
                    "CFG-S011",
                    &path,
                    format!(
                        "storage.libraries[{i}].path '{text}' is already listed as library {j} \
                         (paths are compared without case and slash direction)"
                    ),
                );
            }
            seen.push((key, i));
        }

        if lib.label.chars().count() > MAX_LIBRARY_LABEL_CHARS {
            error(
                out,
                "CFG-S013",
                &format!("storage.libraries[{i}].label"),
                format!(
                    "storage.libraries[{i}].label is {} characters. The limit is \
                     {MAX_LIBRARY_LABEL_CHARS}",
                    lib.label.chars().count()
                ),
            );
        }
    }

    // Nesting: a folder inside another scanned folder would be indexed twice. Only enabled
    // libraries are scanned, so only they count. Equal folders are already CFG-S011.
    let mut scanned: Vec<(String, String, String)> =
        vec![(root_key, "storage.root".to_owned(), root_text)];
    for (i, lib) in libs.iter().enumerate().filter(|(_, l)| l.enabled) {
        let key = library_key(&lib.path);
        if !key.is_empty() {
            scanned.push((
                key,
                format!("storage.libraries[{i}].path"),
                lib.path.trim().to_owned(),
            ));
        }
    }
    for (outer_key, _, outer_text) in &scanned {
        if outer_key.is_empty() {
            continue;
        }
        let prefix = format!("{outer_key}\\");
        for (inner_key, inner_path, inner_text) in &scanned {
            if inner_key != outer_key && inner_key.starts_with(&prefix) {
                warning(
                    out,
                    "CFG-S014",
                    inner_path,
                    format!(
                        "'{inner_text}' is inside '{outer_text}'. Files in it could be indexed twice"
                    ),
                );
            }
        }
    }
}

/// Checks the filename template: illegal characters, tokens, rendered length, uniqueness.
fn check_template(template: &str, out: &mut Vec<Diagnostic>) {
    const PATH: &str = "storage.filename_template";
    if let Some(ch) = template.chars().find(|c| ILLEGAL_NAME_CHARS.contains(c)) {
        error(
            out,
            "CFG-S003",
            PATH,
            format!(
                "storage.filename_template contains '{ch}'. Windows file names cannot contain \
                 < > : \" / \\ | ? *"
            ),
        );
    }

    let mut unique = false;
    let mut has_kind = false;
    // Estimated rendered length: fixed widths for {game} and {kind} and {id}, literal width
    // for date tokens. Good enough to catch templates that are clearly too long.
    let mut rendered = 0usize;
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        rendered += rest[..open].chars().count();
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            error(out, "CFG-S005", PATH, unknown_token_message(after));
            return;
        };
        let tok = &after[..close];
        rest = &after[close + 1..];
        match tok {
            "game" => rendered += 32,
            "kind" => {
                rendered += 7;
                has_kind = true;
            }
            "id" => {
                rendered += 8;
                unique = true;
            }
            _ if is_date_token(tok) => {
                rendered += tok.chars().count();
                unique = true;
            }
            _ => error(out, "CFG-S005", PATH, unknown_token_message(tok)),
        }
    }
    rendered += rest.chars().count();

    if rendered > MAX_RENDERED_NAME {
        error(
            out,
            "CFG-S004",
            PATH,
            format!(
                "storage.filename_template renders to {rendered} characters; the limit is \
                 {MAX_RENDERED_NAME}"
            ),
        );
    }
    if !unique {
        error(
            out,
            "CFG-S008",
            PATH,
            "storage.filename_template needs a unique part: a date token such as \
             {yyyy-MM-dd HH-mm-ss} or {id}"
                .to_owned(),
        );
    }
    if !has_kind {
        warning(
            out,
            "CFG-S007",
            PATH,
            "Without {kind}, session and clip files may look alike. Collisions are numbered (2), (3)"
                .to_owned(),
        );
    }
}

/// Message for CFG-S005. `tok` is the text between the braces.
fn unknown_token_message(tok: &str) -> String {
    format!(
        "storage.filename_template has unknown token {{{tok}}}. Allowed: {{game}}, {{kind}}, \
         {{id}} and date tokens yyyy MM dd HH mm ss"
    )
}

/// True for a date pattern made of yyyy, MM, dd, HH, mm and ss joined by separators.
fn is_date_token(tok: &str) -> bool {
    let mut runs = 0usize;
    for run in tok.split(|c: char| !c.is_ascii_alphabetic()) {
        if run.is_empty() {
            continue;
        }
        if !matches!(run, "yyyy" | "MM" | "dd" | "HH" | "mm" | "ss") {
            return false;
        }
        runs += 1;
    }
    runs > 0
        && tok
            .chars()
            .all(|c| c.is_ascii_alphabetic() || matches!(c, '-' | '_' | ' ' | '.' | ':'))
}

fn check_detection(config: &Config, out: &mut Vec<Diagnostic>) {
    let d = &config.detection;
    for exe in &d.allow {
        if d.block.contains(exe) {
            error(
                out,
                "CFG-D002",
                "detection.allow",
                format!("'{exe}' is in both detection.allow and detection.block"),
            );
        }
    }
    if d.mode == DetectionMode::Allowlist && d.allow.is_empty() {
        warning(
            out,
            "CFG-D003",
            "detection.mode",
            "detection.mode = \"allowlist\" with no entries records nothing".to_owned(),
        );
    }
    if !in_range(d.stop_grace_s, 0, 300) {
        error(
            out,
            "CFG-D004",
            "detection.stop_grace_s",
            format!(
                "detection.stop_grace_s = {} is outside 0..=300",
                d.stop_grace_s
            ),
        );
    }
}

fn check_video(config: &Config, out: &mut Vec<Diagnostic>) {
    let v = &config.video;
    if v.codec == Codec::Av1 && v.encoder == EncoderChoice::Software {
        error(
            out,
            "CFG-V001",
            "video.codec",
            "video.codec = \"av1\" needs a hardware encoder. Set encoder to auto, nvenc, amf or qsv"
                .to_owned(),
        );
    }
    let (codec_name, max_cq) = match v.codec {
        Codec::H264 => ("h264", 51),
        Codec::Hevc => ("hevc", 51),
        Codec::Av1 => ("av1", 63),
    };
    if !in_range(v.quality_cq, 0, max_cq) {
        error(
            out,
            "CFG-V004",
            "video.quality_cq",
            format!(
                "video.quality_cq = {} is outside 0..={max_cq} for codec {codec_name}",
                v.quality_cq
            ),
        );
    }
    check_bitrate(out, "video.bitrate_kbps", v.bitrate_kbps);
    check_bitrate(out, "video.max_bitrate_kbps", v.max_bitrate_kbps);
    // Only the custom preset with VBR rate control uses max_bitrate_kbps (schema.rs).
    if v.preset == Preset::Custom
        && v.rate_control == RateControl::Vbr
        && v.max_bitrate_kbps < v.bitrate_kbps
    {
        error(
            out,
            "CFG-V006",
            "video.max_bitrate_kbps",
            format!(
                "video.max_bitrate_kbps ({}) must be at least video.bitrate_kbps ({})",
                v.max_bitrate_kbps, v.bitrate_kbps
            ),
        );
    }
    if !in_range(v.keyframe_interval_s, 1, 4) {
        error(
            out,
            "CFG-V008",
            "video.keyframe_interval_s",
            format!(
                "video.keyframe_interval_s = {} is outside 1..=4",
                v.keyframe_interval_s
            ),
        );
    }
    if v.gpu.trim().is_empty() {
        error(
            out,
            "CFG-V012",
            "video.gpu",
            "video.gpu is empty. Use \"auto\" or an adapter name from the GPU list".to_owned(),
        );
    }
    if !in_range(v.b_frames, 0, MAX_B_FRAMES) {
        error(
            out,
            "CFG-V013",
            "video.b_frames",
            format!("video.b_frames = {} is outside 0..=4", v.b_frames),
        );
    }
    if v.encoder == EncoderChoice::Software
        && v.fps == Fps::F120
        && v.resolution == Resolution::Native
    {
        warning(
            out,
            "CFG-V010",
            "video.fps",
            "Software encoding at 120 fps and native resolution may exceed the CPU budget and drop frames"
                .to_owned(),
        );
    }
}

/// Range check for a bitrate in kbps (CFG-V005).
fn check_bitrate(out: &mut Vec<Diagnostic>, path: &str, kbps: u32) {
    if !in_range(kbps, 1000, 100_000) {
        error(
            out,
            "CFG-V005",
            path,
            format!("{path} = {kbps} is outside 1000..=100000"),
        );
    }
}

/// Most push-to-talk keys that can be bound at once.
const MAX_PTT_KEYS: usize = 4;

/// Longest allowed push-to-talk release tail, in milliseconds.
const PTT_RELEASE_MAX_MS: u32 = 2000;

fn check_audio(config: &Config, out: &mut Vec<Diagnostic>) {
    let a = &config.audio;
    if a.codec == AudioCodec::Opus {
        error(
            out,
            "CFG-A011",
            "audio.codec",
            "audio.codec = \"opus\" is not supported in v1 (DESIGN.md D1). Use \"aac\"".to_owned(),
        );
    }
    if !in_range(a.bitrate_kbps, 128, 320) {
        error(
            out,
            "CFG-A008",
            "audio.bitrate_kbps",
            format!(
                "audio.bitrate_kbps = {} is outside 128..=320",
                a.bitrate_kbps
            ),
        );
    }
    check_gain(out, "audio.game.gain_db", a.game.gain_db);
    check_gain(out, "audio.mic.gain_db", a.mic.gain_db);
    check_gain(out, "audio.other_apps.gain_db", a.other_apps.gain_db);
    if !in_range(a.mic.gate_db, GATE_DB_RANGE.0, GATE_DB_RANGE.1) {
        error(
            out,
            "CFG-A013",
            "audio.mic.gate_db",
            format!(
                "audio.mic.gate_db = {:.1} is outside -100.0..=-20.0",
                a.mic.gate_db
            ),
        );
    }
    if a.mic.ptt_release_ms > PTT_RELEASE_MAX_MS {
        error(
            out,
            "CFG-A014",
            "audio.mic.ptt_release_ms",
            format!(
                "audio.mic.ptt_release_ms = {} is outside 0..={PTT_RELEASE_MAX_MS}",
                a.mic.ptt_release_ms
            ),
        );
    }
    if a.mic.ptt_separate_track {
        let session = &config.hotkeys.session;
        let ptt_mode = !session.push_to_talk.is_empty()
            || (session.mute_mic_mode == MuteMode::PushToTalk && session.mute_mic.is_some());
        let problem = if !a.mic.enabled {
            Some("audio.mic.enabled = true")
        } else if !a.separate_tracks {
            Some("audio.separate_tracks = true")
        } else if !ptt_mode {
            Some("a key in hotkeys.session.push_to_talk")
        } else {
            None
        };
        if let Some(needs) = problem {
            error(
                out,
                "CFG-A015",
                "audio.mic.ptt_separate_track",
                format!("audio.mic.ptt_separate_track = true needs {needs}"),
            );
        }
    }
    let n_apps = a.apps.len();
    if n_apps > MAX_AUDIO_APPS {
        error(
            out,
            "CFG-A001",
            "audio.apps",
            format!("audio.apps has {n_apps} entries. Allowed: 0..=16"),
        );
    }
    let n_tracks = a.track_plan().len();
    if n_tracks > MAX_AUDIO_TRACKS {
        error(
            out,
            "CFG-A001",
            "audio.separate_tracks",
            format!(
                "{} enabled audio sources make {n_tracks} tracks. The maximum is {MAX_AUDIO_TRACKS}. \
                 Disable a source, or set audio.separate_tracks = false",
                a.enabled_sources().len()
            ),
        );
    }
    for (i, app) in a.apps.iter().enumerate() {
        check_app(out, i, app, &a.apps[..i]);
    }
}

/// CFG-A006 for one gain value. `path` is the dotted key, for example `audio.game.gain_db`.
/// `path` is the dotted key, for example `audio.game.gain_db`. It is formatted only on error.
fn check_gain(out: &mut Vec<Diagnostic>, path: impl std::fmt::Display, gain: f32) {
    if !in_range(gain, GAIN_DB_RANGE.0, GAIN_DB_RANGE.1) {
        let path = path.to_string();
        error(
            out,
            "CFG-A006",
            &path,
            format!("{path} = {gain:.1} is outside -60.0..=12.0"),
        );
    }
}

/// Checks one `[[audio.apps]]` entry. `earlier` holds the entries before it, for uniqueness.
fn check_app(out: &mut Vec<Diagnostic>, i: usize, app: &AudioApp, earlier: &[AudioApp]) {
    if !in_range(app.name.trim().chars().count(), 1, 32) {
        error(
            out,
            "CFG-A002",
            &format!("audio.apps[{i}].name"),
            format!("audio.apps[{i}].name must be 1..=32 characters"),
        );
    }
    if let Some(j) = earlier
        .iter()
        .position(|e| e.name.eq_ignore_ascii_case(&app.name))
    {
        error(
            out,
            "CFG-A003",
            &format!("audio.apps[{i}].name"),
            format!(
                "audio.apps[{i}].name '{}' is already used by app {j}",
                app.name
            ),
        );
    }
    // ExeName is stored lowercase, so plain equality is the case-insensitive comparison.
    if let Some(j) = earlier.iter().position(|e| e.exe == app.exe) {
        error(
            out,
            "CFG-A012",
            &format!("audio.apps[{i}].exe"),
            format!(
                "audio.apps[{i}].exe '{}' is already listed as app {j} (exe names are case-insensitive)",
                app.exe
            ),
        );
    }
    check_gain(out, format_args!("audio.apps[{i}].gain_db"), app.gain_db);
}

fn check_replay(config: &Config, out: &mut Vec<Diagnostic>) {
    let r = &config.replay;
    if !in_range(r.max_seconds, 30, 3600) {
        error(
            out,
            "CFG-R001",
            "replay.max_seconds",
            format!(
                "replay.max_seconds = {} is outside 30..=3600",
                r.max_seconds
            ),
        );
    }
    if !in_range(r.ram_max_mb, 64, 4096) {
        error(
            out,
            "CFG-R003",
            "replay.ram_max_mb",
            format!("replay.ram_max_mb = {} is outside 64..=4096", r.ram_max_mb),
        );
    }
    if r.storage == ReplayStorage::Ram {
        // Conservative estimate: the video ceiling (the larger of the two bitrates) plus every
        // derived audio track at the audio bitrate. kbps * s / 8 gives kB, so divide by 1024 for MiB.
        let video_kbps = u64::from(config.video.max_bitrate_kbps.max(config.video.bitrate_kbps));
        let audio_tracks = config.audio.track_plan().len() as u64;
        let audio_kbps = u64::from(config.audio.bitrate_kbps) * audio_tracks;
        let total_kbps = video_kbps + audio_kbps;
        let seconds = u64::from(r.max_seconds);
        let need_mb = (total_kbps * seconds).div_ceil(8 * 1024);
        if need_mb > u64::from(r.ram_max_mb) {
            error(
                out,
                "CFG-R002",
                "replay.ram_max_mb",
                format!(
                    "replay.storage = \"ram\" needs about {need_mb} MB for {seconds} s at {:.1} Mbps. \
                     Raise replay.ram_max_mb to {need_mb} or lower replay.max_seconds",
                    total_kbps as f64 / 1000.0
                ),
            );
        }
    }
}

fn check_hotkeys(config: &Config, out: &mut Vec<Diagnostic>) {
    let h = &config.hotkeys;
    let max_seconds = config.replay.max_seconds;
    if h.clips.len() > MAX_CLIP_HOTKEYS {
        error(
            out,
            "CFG-H005",
            "hotkeys.clips",
            format!(
                "hotkeys.clips has {} entries. The maximum is {MAX_CLIP_HOTKEYS}",
                h.clips.len()
            ),
        );
    }
    // Every bound chord seen so far, with its press (None for session hotkeys) and where it is
    // bound, for duplicate detection.
    let mut bound: Vec<(KeyChord, Option<Press>, HotkeySite)> = Vec::new();
    for (i, clip) in h.clips.iter().enumerate() {
        if !in_range(clip.name.chars().count(), 1, 32) {
            error(
                out,
                "CFG-H007",
                &format!("hotkeys.clips[{i}].name"),
                format!("hotkeys.clips[{i}].name must be 1..=32 characters"),
            );
        }
        if clip.seconds > max_seconds {
            error(
                out,
                "CFG-H004",
                &format!("hotkeys.clips[{i}].seconds"),
                format!(
                    "hotkeys.clips[{i}].seconds = {} is more than replay.max_seconds = {max_seconds}. \
                     Lower seconds or raise replay.max_seconds",
                    clip.seconds
                ),
            );
        } else if clip.seconds < 5 {
            error(
                out,
                "CFG-H004",
                &format!("hotkeys.clips[{i}].seconds"),
                format!(
                    "hotkeys.clips[{i}].seconds = {} is outside 5..=3600",
                    clip.seconds
                ),
            );
        }
        check_chord_named(out, &clip.keys, &format!("hotkeys.clips[{i}].keys"));
        if is_reserved(&clip.keys) {
            error(
                out,
                "CFG-H003",
                &format!("hotkeys.clips[{i}].keys"),
                format!(
                    "hotkeys: '{}' is reserved by Windows or ClipCheck (for example Alt+F4, Win+L, \
                     Ctrl+Alt+Del)",
                    chord_text(&clip.keys)
                ),
            );
        }
        add_binding(
            out,
            &mut bound,
            clip.keys,
            Some(clip.press),
            HotkeySite::Clip {
                index: i,
                name: &clip.name,
                press: clip.press,
            },
        );
    }

    let s = &h.session;
    let session = [
        ("split", s.split),
        ("stop_start", s.stop_start),
        ("toggle_buffer", s.toggle_buffer),
        ("mute_mic", s.mute_mic),
    ];
    for (name, chord) in session {
        if let Some(chord) = chord {
            let path = format!("hotkeys.session.{name}");
            check_chord_named(out, &chord, &path);
            if is_reserved(&chord) {
                error(
                    out,
                    "CFG-H003",
                    &path,
                    format!(
                        "hotkeys: '{}' is reserved by Windows or ClipCheck (for example Alt+F4, Win+L, \
                         Ctrl+Alt+Del)",
                        chord_text(&chord)
                    ),
                );
            }
            add_binding(out, &mut bound, chord, None, HotkeySite::Session(name));
        }
    }
    if s.push_to_talk.len() > MAX_PTT_KEYS {
        error(
            out,
            "CFG-H009",
            "hotkeys.session.push_to_talk",
            format!(
                "hotkeys.session.push_to_talk has {} keys. Allowed: 0..={MAX_PTT_KEYS}",
                s.push_to_talk.len()
            ),
        );
    }
    for (i, chord) in s.push_to_talk.iter().enumerate() {
        let path = format!("hotkeys.session.push_to_talk[{i}]");
        check_chord_named(out, chord, &path);
        if is_reserved(chord) {
            error(
                out,
                "CFG-H003",
                &path,
                format!(
                    "hotkeys: '{}' is reserved by Windows or ClipCheck (for example Alt+F4, Win+L, \n                     Ctrl+Alt+Del)",
                    chord_text(chord)
                ),
            );
        }
        add_binding(out, &mut bound, *chord, None, HotkeySite::PushToTalk(i));
    }
    if s.mute_mic_mode == MuteMode::PushToTalk && s.mute_mic.is_none() {
        error(
            out,
            "CFG-H008",
            "hotkeys.session.mute_mic_mode",
            "mute_mic_mode = \"push_to_talk\" needs hotkeys.session.mute_mic to be set".to_owned(),
        );
    }
}

/// Where a hotkey is configured. Labels and paths are formatted only when a diagnostic needs them.
#[derive(Clone, Copy)]
enum HotkeySite<'a> {
    /// `hotkeys.clips[index].keys`, named `name` in the label, fired by `press`.
    Clip {
        index: usize,
        name: &'a str,
        press: Press,
    },
    /// `hotkeys.session.<name>`.
    Session(&'static str),
    /// `hotkeys.session.push_to_talk[index]`.
    PushToTalk(usize),
}

impl HotkeySite<'_> {
    /// The human label ("the split hotkey") and the dotted config path.
    fn label_and_path(&self) -> (String, String) {
        match *self {
            HotkeySite::Clip { index, name, press } => {
                let word = match press {
                    Press::Tap => "tap",
                    Press::Long => "long press",
                };
                (
                    format!("the {name} hotkey ({word})"),
                    format!("hotkeys.clips[{index}].keys"),
                )
            }
            HotkeySite::Session(name) => (
                format!("the session {name} hotkey"),
                format!("hotkeys.session.{name}"),
            ),
            HotkeySite::PushToTalk(index) => (
                "a push-to-talk hotkey".to_owned(),
                format!("hotkeys.session.push_to_talk[{index}]"),
            ),
        }
    }
}

/// Records `chord` as bound at `site` and reports CFG-H002 when it is already bound.
///
/// `press` is `Some` for a clip and `None` for a session hotkey. An entry conflicts with an
/// earlier one on the same chord unless both are clips with different presses, so one tap
/// clip and one long clip may share a chord.
fn add_binding<'a>(
    out: &mut Vec<Diagnostic>,
    bound: &mut Vec<(KeyChord, Option<Press>, HotkeySite<'a>)>,
    chord: KeyChord,
    press: Option<Press>,
    site: HotkeySite<'a>,
) {
    let conflicts = |other: Option<Press>| match (other, press) {
        (Some(a), Some(b)) => a == b,
        _ => true,
    };
    if let Some((_, _, other)) = bound.iter().find(|(c, p, _)| *c == chord && conflicts(*p)) {
        let (other_label, _) = other.label_and_path();
        let (label, path) = site.label_and_path();
        error(
            out,
            "CFG-H002",
            &path,
            format!(
                "hotkeys: '{}' is assigned to both {other_label} and {label}",
                chord_text(&chord)
            ),
        );
    }
    bound.push((chord, press, site));
}

/// CFG-H001 when the chord's key cannot be written to config: a refused chord (Escape,
/// Alt+Space) or a key code with no name.
fn check_chord_named(out: &mut Vec<Diagnostic>, chord: &KeyChord, path: &str) {
    if chord.has_key_name() {
        return;
    }
    let message = match chord.refusal() {
        Some(reason) => format!("hotkeys: {reason}. Choose another key at {path}"),
        None => format!(
            "hotkeys: key code 0x{:02X} at {path} is not a valid key chord. Use Ctrl, Alt, \
             Shift or Win plus a named key such as A, F9, Pause or Numpad5",
            chord.vk
        ),
    };
    error(out, "CFG-H001", path, message);
}

/// Chords reserved for Windows or ClipCheck (CFG-H003): Alt+F4, Win+L, Ctrl+Alt+Delete,
/// Alt+Tab and Win+PrintScreen. Escape and Alt+Space are refused at parse time instead.
fn is_reserved(chord: &KeyChord) -> bool {
    let m = chord.mods;
    let only = |ctrl: bool, alt: bool, win: bool| {
        m.ctrl == ctrl && m.alt == alt && m.win == win && !m.shift
    };
    (only(false, true, false) && chord.vk == VK_F4)
        || (only(false, false, true) && chord.vk == VK_L)
        || (only(true, true, false) && chord.vk == VK_DELETE)
        || (only(false, true, false) && chord.vk == VK_TAB)
        || (only(false, false, true) && chord.vk == VK_PRINT_SCREEN)
}

/// Display text of a chord, such as "Alt+F9". Falls back to the raw key code.
fn chord_text(chord: &KeyChord) -> String {
    let mut text = String::new();
    if write!(text, "{chord}").is_err() {
        text = format!("VK 0x{:X}", chord.vk);
    }
    text
}

fn check_notifications(config: &Config, out: &mut Vec<Diagnostic>) {
    let n = &config.notifications;
    if !in_range(n.low_disk_gb, 1, 100) {
        error(
            out,
            "CFG-N001",
            "notifications.low_disk_gb",
            format!(
                "notifications.low_disk_gb = {} is outside 1..=100",
                n.low_disk_gb
            ),
        );
    }
}

fn check_advanced(config: &Config, out: &mut Vec<Diagnostic>) {
    let a = &config.advanced;
    if !in_range(a.detect_poll_ms, 250, 5000) {
        error(
            out,
            "CFG-X001",
            "advanced.detect_poll_ms",
            format!(
                "advanced.detect_poll_ms = {} is outside 250..=5000",
                a.detect_poll_ms
            ),
        );
    }
    if !in_range(a.ipc_timeout_ms, 500, 10_000) {
        error(
            out,
            "CFG-X005",
            "advanced.ipc_timeout_ms",
            format!(
                "advanced.ipc_timeout_ms = {} is outside 500..=10000",
                a.ipc_timeout_ms
            ),
        );
    }
}

/// CFG-X006 (interval range) and CFG-X007 (endpoint format) for `[updates]`.
fn check_updates(config: &Config, out: &mut Vec<Diagnostic>) {
    let u = &config.updates;
    if !in_range(
        u.interval_hours,
        UPDATE_INTERVAL_HOURS_MIN,
        UPDATE_INTERVAL_HOURS_MAX,
    ) {
        error(
            out,
            "CFG-X006",
            "updates.interval_hours",
            format!(
                "updates.interval_hours = {} is outside {UPDATE_INTERVAL_HOURS_MIN}..={UPDATE_INTERVAL_HOURS_MAX}",
                u.interval_hours
            ),
        );
    }
    if !is_valid_update_endpoint(&u.endpoint) {
        error(
            out,
            "CFG-X007",
            "updates.endpoint",
            format!(
                "updates.endpoint '{}' is not allowed. Use an https:// URL with no spaces, at most \
                 {MAX_UPDATE_ENDPOINT_CHARS} characters (http://127.0.0.1 or http://localhost are \
                 accepted for local testing), or leave it empty for the built-in URL",
                u.endpoint
            ),
        );
    }
}

/// True for an empty endpoint (built-in default), or a URL with no whitespace, at most
/// [`MAX_UPDATE_ENDPOINT_CHARS`] characters, that starts with `https://` followed by a host, or
/// with `http://127.0.0.1` / `http://localhost` (local testing only) followed by end, `/` or `:`.
fn is_valid_update_endpoint(endpoint: &str) -> bool {
    if endpoint.is_empty() {
        return true;
    }
    if endpoint.chars().count() > MAX_UPDATE_ENDPOINT_CHARS
        || endpoint.chars().any(char::is_whitespace)
    {
        return false;
    }
    if let Some(rest) = endpoint.strip_prefix("https://") {
        return !rest.is_empty() && !rest.starts_with('/');
    }
    ["http://127.0.0.1", "http://localhost"].iter().any(|base| {
        endpoint.strip_prefix(base).is_some_and(|after| {
            after.is_empty() || after.starts_with('/') || after.starts_with(':')
        })
    })
}

fn check_games(config: &Config, out: &mut Vec<Diagnostic>) {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for (id, g) in &config.games {
        match normalize_game_key(id) {
            None => {
                let message = if id.trim_start().to_ascii_lowercase().starts_with("exe:") {
                    format!("games key '{id}' uses an 'exe:' prefix, which is not a game key. Use 'name.exe' or 'name.exe@store:id'")
                } else {
                    format!("games key '{id}' is not a valid game id. Use 'name.exe' or 'name.exe@store:id'")
                };
                error(out, "CFG-G001", &format!("games.\"{id}\""), message);
            }
            Some(norm) => {
                if !seen.insert(norm.clone()) {
                    error(
                        out,
                        "CFG-G001",
                        &format!("games.\"{id}\""),
                        format!("games key '{id}' duplicates '{norm}' after normalization. Keep one entry per game"),
                    );
                }
            }
        }
        if let Some(name) = &g.display_name {
            if !in_range(name.chars().count(), 1, 64) {
                game_error(out, id, "display_name", "must be 1..=64 characters");
            }
        }
        if let Some(s) = g.replay_max_seconds {
            if !in_range(s, 30, 3600) {
                game_error(
                    out,
                    id,
                    "replay_max_seconds",
                    &format!("{s} is outside 30..=3600"),
                );
            }
        }
        if let Some(folder) = &g.folder {
            let text = folder.to_string_lossy();
            if !text.contains('%') && !folder.is_absolute() {
                game_error(out, id, "folder", "must be an absolute path");
            }
        }
    }
}

/// CFG-G002 for an override whose merged value is invalid.
fn game_error(out: &mut Vec<Diagnostic>, id: &str, key: &str, detail: &str) {
    error(
        out,
        "CFG-G002",
        &format!("games.\"{id}\".{key}"),
        format!("games.'{id}' results in an invalid {key}: {detail}"),
    );
}

#[cfg(test)]
mod hotkey_site_tests {
    use super::{validate, KeyChord};
    use crate::config::schema::{ClipHotkey, Config, Press};

    #[test]
    fn unnamed_session_key_is_cfg_h001_not_a_panic() {
        let mut config = Config::default();
        config.hotkeys.session.split = Some(KeyChord::alt(0x07));
        let diags = validate(&config);
        let d = diags
            .iter()
            .find(|d| d.code == "CFG-H001")
            .expect("CFG-H001 reported");
        assert_eq!(d.path, "hotkeys.session.split");
        assert!(d.message.contains("0x07"), "{}", d.message);
    }

    #[test]
    fn duplicate_clip_and_session_chord_names_both_labels() {
        let mut config = Config::default();
        config.hotkeys.clips.clear();
        let chord: KeyChord = "Alt+F9".parse().expect("parses");
        config.hotkeys.session.split = Some(chord);
        config.hotkeys.clips.push(ClipHotkey {
            name: "Big".to_owned(),
            seconds: 30,
            keys: chord,
            press: Press::Tap,
        });
        let diags = validate(&config);
        let d = diags
            .iter()
            .find(|d| d.code == "CFG-H002")
            .expect("CFG-H002 reported");
        assert_eq!(d.path, "hotkeys.session.split");
        assert_eq!(
            d.message,
            "hotkeys: 'Alt+F9' is assigned to both the Big hotkey (tap) and the session split hotkey"
        );
    }
}
