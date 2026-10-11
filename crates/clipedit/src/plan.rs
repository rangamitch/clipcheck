//! Trim planner: pure logic that turns a requested range plus keyframe times
//! into the output range and the work the encoder/muxer must do.
//!
//! No I/O and no external tools. See `docs/design/08-player-and-clip-editor.md`
//! sections 4.5, 5.3 and 5.4.

use std::fmt;

/// Minimum allowed selection length in milliseconds.
///
/// The design doc does not state a value, so 100 ms is used here. Revisit if
/// the doc gains a number.
pub const MIN_SELECTION_MS: i64 = 100;

/// Output kind used for editor outputs (DESIGN.md section 8, D5).
pub const CLIP_TRIM_KIND: &str = "clip-trim";

/// How the trim is performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimMode {
    /// Stream copy. Start snaps back to the previous keyframe; end is packet accurate.
    Lossless,
    /// Re-encode video from the previous keyframe and discard frames before the
    /// requested start. Audio is stream-copied from the requested start.
    Precise,
}

/// Inputs to [`plan_trim`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrimInput<'a> {
    /// Video keyframe presentation times in ms, sorted ascending.
    pub keyframes_ms: &'a [i64],
    /// Source duration in ms.
    pub duration_ms: i64,
    /// Requested in-point in ms.
    pub start_ms: i64,
    /// Requested out-point in ms (exclusive).
    pub end_ms: i64,
    /// Requested trim mode.
    pub mode: TrimMode,
}

/// Result of [`plan_trim`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrimPlan {
    /// Mode the plan was built for.
    pub mode: TrimMode,
    /// Requested in-point in ms.
    pub requested_start_ms: i64,
    /// Requested out-point in ms.
    pub requested_end_ms: i64,
    /// Output in-point in ms. Lossless: keyframe at or before the request.
    pub actual_start_ms: i64,
    /// Output out-point in ms. Equal to the requested end.
    pub actual_end_ms: i64,
    /// `requested_start_ms - actual_start_ms`, always >= 0. Shown in the UI as "start snapped back".
    pub snap_delta_ms: i64,
    /// Precise mode: keyframe where video decoding/re-encoding begins. `None` for lossless.
    pub reencode_from_keyframe_ms: Option<i64>,
    /// Precise mode: decoded video before this offset from the re-encode start is discarded.
    /// Always `requested_start_ms - reencode_from_keyframe_ms`. Zero for lossless.
    pub discard_head_ms: i64,
}

impl TrimPlan {
    /// Length of the output in ms.
    pub fn output_duration_ms(&self) -> i64 {
        self.actual_end_ms - self.actual_start_ms
    }
}

/// Errors returned by [`plan_trim`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrimError {
    /// Duration is zero or negative.
    InvalidDuration { duration_ms: i64 },
    /// Start or end lies outside `[0, duration_ms]`.
    OutsideDuration {
        start_ms: i64,
        end_ms: i64,
        duration_ms: i64,
    },
    /// Start is not before end.
    StartNotBeforeEnd { start_ms: i64, end_ms: i64 },
    /// Selection is shorter than [`MIN_SELECTION_MS`].
    TooShort { min_ms: i64, actual_ms: i64 },
    /// Keyframe list is not sorted ascending.
    UnsortedKeyframes,
    /// Lossless mode needs a keyframe index. Precise mode does not.
    NoKeyframes,
}

impl fmt::Display for TrimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrimError::InvalidDuration { duration_ms } => {
                write!(f, "invalid duration {duration_ms} ms")
            }
            TrimError::OutsideDuration {
                start_ms,
                end_ms,
                duration_ms,
            } => {
                write!(
                    f,
                    "range {start_ms}..{end_ms} is outside duration {duration_ms}"
                )
            }
            TrimError::StartNotBeforeEnd { start_ms, end_ms } => {
                write!(f, "start {start_ms} must be before end {end_ms}")
            }
            TrimError::TooShort { min_ms, actual_ms } => {
                write!(
                    f,
                    "selection {actual_ms} ms is shorter than the {min_ms} ms minimum"
                )
            }
            TrimError::UnsortedKeyframes => write!(f, "keyframe list is not sorted"),
            TrimError::NoKeyframes => write!(f, "lossless trim needs a keyframe index"),
        }
    }
}

impl std::error::Error for TrimError {}

/// Largest keyframe at or before `t_ms`. `None` if every keyframe is after `t_ms`.
fn floor_keyframe(keyframes_ms: &[i64], t_ms: i64) -> Option<i64> {
    let idx = keyframes_ms.partition_point(|&k| k <= t_ms);
    if idx == 0 {
        None
    } else {
        Some(keyframes_ms[idx - 1])
    }
}

/// Plans a trim. Validates the range, then snaps (lossless) or computes the
/// re-encode window (precise).
///
/// Lossless: `actual_start_ms` = previous keyframe (or 0 when the request is
/// before the first keyframe); `actual_end_ms` = requested end.
///
/// Precise: no snapping. `reencode_from_keyframe_ms` = previous keyframe (or 0),
/// `discard_head_ms` = the gap between that keyframe and the requested start.
pub fn plan_trim(input: &TrimInput<'_>) -> Result<TrimPlan, TrimError> {
    let TrimInput {
        keyframes_ms,
        duration_ms,
        start_ms,
        end_ms,
        mode,
    } = *input;

    if duration_ms <= 0 {
        return Err(TrimError::InvalidDuration { duration_ms });
    }
    if start_ms < 0 || end_ms > duration_ms {
        return Err(TrimError::OutsideDuration {
            start_ms,
            end_ms,
            duration_ms,
        });
    }
    if start_ms >= end_ms {
        return Err(TrimError::StartNotBeforeEnd { start_ms, end_ms });
    }
    let len = end_ms - start_ms;
    if len < MIN_SELECTION_MS {
        return Err(TrimError::TooShort {
            min_ms: MIN_SELECTION_MS,
            actual_ms: len,
        });
    }
    if keyframes_ms.windows(2).any(|w| w[0] > w[1]) {
        return Err(TrimError::UnsortedKeyframes);
    }

    let floor = floor_keyframe(keyframes_ms, start_ms);

    match mode {
        TrimMode::Lossless => {
            if keyframes_ms.is_empty() {
                return Err(TrimError::NoKeyframes);
            }
            let actual_start_ms = floor.unwrap_or(0);
            Ok(TrimPlan {
                mode,
                requested_start_ms: start_ms,
                requested_end_ms: end_ms,
                actual_start_ms,
                actual_end_ms: end_ms,
                snap_delta_ms: start_ms - actual_start_ms,
                reencode_from_keyframe_ms: None,
                discard_head_ms: 0,
            })
        }
        TrimMode::Precise => {
            let from = floor.unwrap_or(0);
            Ok(TrimPlan {
                mode,
                requested_start_ms: start_ms,
                requested_end_ms: end_ms,
                actual_start_ms: start_ms,
                actual_end_ms: end_ms,
                snap_delta_ms: 0,
                reencode_from_keyframe_ms: Some(from),
                discard_head_ms: start_ms - from,
            })
        }
    }
}

/// Splits a millisecond time into `(hours, minutes, seconds, milliseconds)`.
/// Negative values are clamped to zero. Hours are not capped at 99.
fn hms_parts(ms: i64) -> (i64, i64, i64, i64) {
    let ms = ms.max(0);
    (
        ms / 3_600_000,
        (ms / 60_000) % 60,
        (ms / 1_000) % 60,
        ms % 1_000,
    )
}

/// Formats a millisecond time as `HH:MM:SS.mmm` for the editor's time labels.
/// Negative values are clamped to zero. Hours are not capped at 99.
pub fn format_hms(ms: i64) -> String {
    let (h, m, s, frac) = hms_parts(ms);
    format!("{h:02}:{m:02}:{s:02}.{frac:03}")
}

/// Filename-safe form of [`format_hms`] (`HH-MM-SS`, no colons or dots).
fn format_hms_file(ms: i64) -> String {
    let (h, m, s, _) = hms_parts(ms);
    format!("{h:02}-{m:02}-{s:02}")
}

/// True for a trailing kind token such as `clip-30s`, `clip-trim` or `session-part3`.
fn is_kind_token(token: &str) -> bool {
    if token == "clip-30s" || token == CLIP_TRIM_KIND {
        return true;
    }
    match token.strip_prefix("session-part") {
        Some(n) => !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()),
        None => false,
    }
}

/// Builds an output file name for a trim of `original_name`, of the form
/// `{base} {HH-MM-SS}-{HH-MM-SS} clip-trim`, with no extension.
///
/// The base is the original file name with its directory, extension and any
/// trailing kind token removed. Characters that Windows forbids in file names
/// are dropped. The range is the trim's start and end on the source timeline.
/// The caller adds the extension and resolves collisions.
pub fn suggest_output_name(original_name: &str, start_ms: i64, end_ms: i64) -> String {
    let file = original_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(original_name);
    let stem = match file.rfind('.') {
        Some(i) if i > 0 => &file[..i],
        _ => file,
    };

    let mut base = stem.trim();
    while let Some((head, last)) = base.rsplit_once(' ') {
        if is_kind_token(last) {
            base = head.trim_end();
        } else {
            break;
        }
    }
    let cleaned: String = base
        .chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').trim_end();
    let base = if cleaned.is_empty() { "clip" } else { cleaned };

    format!(
        "{base} {}-{} {CLIP_TRIM_KIND}",
        format_hms_file(start_ms),
        format_hms_file(end_ms)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(kf: &'a [i64], start: i64, end: i64, mode: TrimMode) -> TrimInput<'a> {
        TrimInput {
            keyframes_ms: kf,
            duration_ms: 60_000,
            start_ms: start,
            end_ms: end,
            mode,
        }
    }

    #[test]
    fn lossless_snaps_start_back() {
        let kf = [0, 2_000, 4_000, 6_000];
        let p = plan_trim(&input(&kf, 5_500, 10_000, TrimMode::Lossless)).unwrap();
        assert_eq!(p.actual_start_ms, 4_000);
        assert_eq!(p.snap_delta_ms, 1_500);
        assert_eq!(p.actual_end_ms, 10_000);
        assert_eq!(p.reencode_from_keyframe_ms, None);
        assert_eq!(p.discard_head_ms, 0);
    }

    #[test]
    fn precise_reports_window() {
        let kf = [0, 2_000, 4_000];
        let p = plan_trim(&input(&kf, 5_500, 10_000, TrimMode::Precise)).unwrap();
        assert_eq!(p.actual_start_ms, 5_500);
        assert_eq!(p.snap_delta_ms, 0);
        assert_eq!(p.reencode_from_keyframe_ms, Some(4_000));
        assert_eq!(p.discard_head_ms, 1_500);
    }

    #[test]
    fn precise_without_keyframes_is_allowed() {
        let p = plan_trim(&input(&[], 1_000, 5_000, TrimMode::Precise)).unwrap();
        assert_eq!(p.reencode_from_keyframe_ms, Some(0));
        assert_eq!(p.discard_head_ms, 1_000);
    }

    #[test]
    fn lossless_without_keyframes_errors() {
        assert_eq!(
            plan_trim(&input(&[], 1_000, 5_000, TrimMode::Lossless)),
            Err(TrimError::NoKeyframes)
        );
    }

    #[test]
    fn start_before_first_keyframe_snaps_to_zero() {
        let kf = [1_000, 3_000];
        let p = plan_trim(&input(&kf, 500, 5_000, TrimMode::Lossless)).unwrap();
        assert_eq!(p.actual_start_ms, 0);
        assert_eq!(p.snap_delta_ms, 500);
    }

    #[test]
    fn exact_keyframe_start_has_zero_snap() {
        let kf = [0, 2_000];
        let p = plan_trim(&input(&kf, 2_000, 5_000, TrimMode::Lossless)).unwrap();
        assert_eq!(p.snap_delta_ms, 0);
    }

    #[test]
    fn validation_errors() {
        let kf = [0];
        assert_eq!(
            plan_trim(&input(&kf, 5_000, 5_000, TrimMode::Lossless)),
            Err(TrimError::StartNotBeforeEnd {
                start_ms: 5_000,
                end_ms: 5_000
            })
        );
        assert_eq!(
            plan_trim(&input(&kf, -1, 5_000, TrimMode::Lossless)),
            Err(TrimError::OutsideDuration {
                start_ms: -1,
                end_ms: 5_000,
                duration_ms: 60_000
            })
        );
        assert_eq!(
            plan_trim(&input(&kf, 0, 60_001, TrimMode::Lossless)),
            Err(TrimError::OutsideDuration {
                start_ms: 0,
                end_ms: 60_001,
                duration_ms: 60_000
            })
        );
        assert_eq!(
            plan_trim(&input(&kf, 1_000, 1_050, TrimMode::Precise)),
            Err(TrimError::TooShort {
                min_ms: MIN_SELECTION_MS,
                actual_ms: 50
            })
        );
        assert_eq!(
            plan_trim(&input(&[5, 1], 0, 5_000, TrimMode::Lossless)),
            Err(TrimError::UnsortedKeyframes)
        );
        let zero = TrimInput {
            keyframes_ms: &kf,
            duration_ms: 0,
            start_ms: 0,
            end_ms: 1_000,
            mode: TrimMode::Precise,
        };
        assert_eq!(
            plan_trim(&zero),
            Err(TrimError::InvalidDuration { duration_ms: 0 })
        );
    }

    #[test]
    fn min_length_boundary_is_inclusive() {
        let kf = [0];
        let end = MIN_SELECTION_MS;
        assert!(plan_trim(&input(&kf, 0, end, TrimMode::Precise)).is_ok());
        assert!(plan_trim(&input(&kf, 0, end - 1, TrimMode::Precise)).is_err());
    }

    #[test]
    fn hms_formatting() {
        assert_eq!(format_hms(0), "00:00:00.000");
        assert_eq!(format_hms(12_204), "00:00:12.204");
        assert_eq!(format_hms(3_723_005), "01:02:03.005");
        assert_eq!(format_hms(-5), "00:00:00.000");
    }

    #[test]
    fn file_form_agrees_with_display_form_on_whole_seconds() {
        for ms in [-5, 0, 999, 12_204, 3_723_005, 36_000_000 + 59_999] {
            let display = format_hms(ms);
            let whole = display.split('.').next().expect("seconds part");
            assert_eq!(format_hms_file(ms), whole.replace(':', "-"), "ms={ms}");
        }
    }

    #[test]
    fn output_name_strips_extension_and_kind() {
        assert_eq!(
            suggest_output_name(
                "Game 2026-10-09 14-02-11 session-part2.mp4",
                754_000,
                1_862_500
            ),
            "Game 2026-10-09 14-02-11 00-12-34-00-31-02 clip-trim"
        );
        assert_eq!(
            suggest_output_name(r"C:\rec\Clip clip-30s.mp4", 0, 1_000),
            "Clip 00-00-00-00-00-01 clip-trim"
        );
    }

    #[test]
    fn output_name_sanitizes_and_falls_back() {
        assert_eq!(
            suggest_output_name("a:b?c.mp4", 0, 1_000),
            "abc 00-00-00-00-00-01 clip-trim"
        );
        assert_eq!(
            suggest_output_name("Clip.mp4", 0, 1_000),
            "Clip 00-00-00-00-00-01 clip-trim"
        );
        assert_eq!(
            suggest_output_name("   .mp4", 0, 1_000),
            "clip 00-00-00-00-00-01 clip-trim"
        );
    }
}
