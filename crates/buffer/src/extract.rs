//! Lossless clip extraction (D19, D58).
//!
//! The segments of a [`ClipPlan`] are joined and trimmed in process by
//! `clipcheck_mp4::remux::concat_remux` (stream copy, no re-encode). The remuxer
//! writes its own temporary file and renames it onto the output only after
//! success, so a failed run never leaves a partial clip at the output path.
//! No child process is spawned here.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use clipcheck_mp4::remux::{concat_remux, RemuxOptions, TrackSelect};
use clipcheck_mp4::Mp4Error;

use crate::index::{ClipPlan, TICKS_PER_SEC};

/// Result of a successful clip extraction.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractReport {
    /// Path of the written clip.
    pub out: PathBuf,
    /// Expected clip duration in seconds, taken from the plan.
    pub duration_secs_expected: f64,
    /// Size of the written clip in bytes.
    pub bytes: u64,
}

/// Builds the body of a concat list file, one `file` line per path. Backslashes
/// become forward slashes, and single quotes are escaped as `'\''`.
pub fn build_concat_list(paths: &[PathBuf]) -> String {
    build_concat_list_trimmed(paths, 0.0)
}

/// Writes a concat list for `paths` to `list`, using [`build_concat_list`]
/// rules. When `trim_secs` is positive the first entry also gets an `inpoint`
/// (see [`build_concat_list_trimmed`]).
pub fn write_concat_list(list: &Path, paths: &[PathBuf], trim_secs: f64) -> io::Result<()> {
    fs::write(list, build_concat_list_trimmed(paths, trim_secs))
}

/// Concatenates the inputs named by the concat list `list` into `out` with
/// stream copy, faststart, and the first segment trimmed by the list's
/// `inpoint` (when present). `duration_secs`, when set, bounds the output to
/// that many seconds after the start.
///
/// The remuxer runs in process and keeps the audio track titles itself. The
/// output is written by the remuxer to a `.part` sibling and renamed onto `out`
/// only on success, so `out` is never left partial. The caller owns `list` and
/// removes it. Returns the size of `out` in bytes.
///
/// Errors: `InvalidInput` when `out` has no file name. Remuxer I/O errors pass
/// through. Other remuxer errors are `InvalidData` with the remuxer's message.
pub fn concat_publish(list: &Path, out: &Path, duration_secs: Option<f64>) -> io::Result<u64> {
    if out.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("output path has no file name: {}", out.display()),
        ));
    }
    let text = fs::read_to_string(list)?;
    let (inputs, start_secs) = parse_concat_list(&text)?;
    let opts = RemuxOptions {
        start_secs,
        end_secs: duration_secs.map(|d| start_secs + d),
        tracks: TrackSelect::All,
        faststart: true,
    };
    let report = concat_remux(&inputs, out, &opts).map_err(mp4_to_io)?;
    Ok(report.bytes)
}

/// Like [`build_concat_list`], but when `trim_secs` is positive the first entry
/// gets an `inpoint` directive, which starts the first segment `trim_secs` in.
fn build_concat_list_trimmed(paths: &[PathBuf], trim_secs: f64) -> String {
    let mut list = String::new();
    for (i, path) in paths.iter().enumerate() {
        let forward = path.to_string_lossy().replace('\\', "/");
        let escaped = forward.replace('\'', r"'\''");
        list.push_str("file '");
        list.push_str(&escaped);
        list.push_str("'\n");
        if i == 0 && trim_secs > 0.0 {
            list.push_str(&format!("inpoint {trim_secs:.6}\n"));
        }
    }
    list
}

/// Parses a concat list written by [`build_concat_list_trimmed`]. Returns the
/// input paths in order and the `inpoint` of the first entry in seconds (0 when
/// there is none). Blank lines and `ffconcat` headers are ignored. Any other
/// line is `InvalidData`.
fn parse_concat_list(text: &str) -> io::Result<(Vec<PathBuf>, f64)> {
    let mut inputs = Vec::new();
    let mut start_secs = 0.0;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line == "ffconcat version 1.0" {
            continue;
        }
        if let Some(body) = line.strip_prefix("file '") {
            let body = body.strip_suffix('\'').ok_or_else(|| bad_list(line))?;
            inputs.push(PathBuf::from(body.replace(r"'\''", "'")));
        } else if let Some(secs) = line.strip_prefix("inpoint ") {
            if !inputs.is_empty() {
                start_secs = secs.trim().parse::<f64>().map_err(|_| bad_list(line))?;
            }
        } else {
            return Err(bad_list(line));
        }
    }
    Ok((inputs, start_secs))
}

fn bad_list(line: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("unsupported concat list line: {line}"),
    )
}

fn mp4_to_io(e: Mp4Error) -> io::Error {
    match e {
        Mp4Error::Io(e) => e,
        other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
    }
}

/// Extracts the clip described by `plan` into `out`, stream copy, in process.
/// The concat list is written to `<out>.concat.txt` and removed before
/// returning. Audio titles are preserved by the remuxer.
///
/// Errors:
/// - `InvalidInput` when the plan has no segments or `out` has no file name.
/// - `NotFound` naming the first segment file that does not exist.
/// - `InvalidData` when the segments cannot be read or remuxed.
pub fn extract_clip(plan: &ClipPlan, out: &Path) -> io::Result<ExtractReport> {
    if plan.segments.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "clip plan has no segments",
        ));
    }
    if out.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("output path has no file name: {}", out.display()),
        ));
    }

    let mut inputs = Vec::with_capacity(plan.segments.len());
    for seg in &plan.segments {
        if !seg.path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("segment file not found: {}", seg.path.display()),
            ));
        }
        inputs.push(absolute(&seg.path)?);
    }

    let list_path = sibling(out, ".concat.txt");

    // The trim is passed as the list's `inpoint` on the first entry. The
    // remuxer snaps the video start back to a keyframe.
    let trim_secs = plan.trim_start_offset_100ns as f64 / TICKS_PER_SEC as f64;
    let result = write_concat_list(&list_path, &inputs, trim_secs)
        .and_then(|()| concat_publish(&list_path, out, Some(plan.effective_secs)));

    let _ = fs::remove_file(&list_path);

    result.map(|bytes| ExtractReport {
        out: out.to_path_buf(),
        duration_secs_expected: plan.effective_secs,
        bytes,
    })
}

/// Returns `out` with `suffix` appended to its file name, in the same directory.
fn sibling(out: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = out.file_name().unwrap_or_default().to_owned();
    name.push(suffix);
    out.with_file_name(name)
}

/// Makes a relative path absolute against the current directory. The list
/// holds absolute paths so that the list's location does not matter.
fn absolute(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(env::current_dir()?.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concat_list_uses_forward_slashes() {
        let list = build_concat_list(&[PathBuf::from(r"C:\buf\seg-000001.mp4")]);
        assert_eq!(list, "file 'C:/buf/seg-000001.mp4'\n");
    }

    #[test]
    fn concat_list_escapes_single_quotes() {
        let list = build_concat_list(&[PathBuf::from("/tmp/it's.mp4")]);
        assert_eq!(list, "file '/tmp/it'\\''s.mp4'\n");
    }

    #[test]
    fn concat_list_empty_input_is_empty() {
        assert_eq!(build_concat_list(&[]), "");
    }

    #[test]
    fn concat_list_round_trips_paths_and_inpoint() {
        let paths = vec![
            PathBuf::from("/buf/it's-000001.mp4"),
            PathBuf::from("/buf/seg-000002.mp4"),
        ];
        let text = build_concat_list_trimmed(&paths, 1.5);
        let (parsed, start) = parse_concat_list(&text).expect("parse");
        assert_eq!(parsed, paths);
        assert!((start - 1.5).abs() < 1e-6);
    }

    #[test]
    fn concat_list_without_trim_starts_at_zero() {
        let text = build_concat_list(&[PathBuf::from("/buf/a.mp4")]);
        let (parsed, start) = parse_concat_list(&text).expect("parse");
        assert_eq!(parsed, vec![PathBuf::from("/buf/a.mp4")]);
        assert_eq!(start, 0.0);
    }

    #[test]
    fn concat_list_skips_headers_and_blank_lines() {
        let text = "ffconcat version 1.0\n\nfile '/buf/a.mp4'\n\n";
        let (parsed, start) = parse_concat_list(text).expect("parse");
        assert_eq!(parsed, vec![PathBuf::from("/buf/a.mp4")]);
        assert_eq!(start, 0.0);
    }

    #[test]
    fn concat_list_rejects_unknown_lines() {
        let err = parse_concat_list("duration 1.0\n").expect_err("unknown directive");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn sibling_appends_suffix_in_same_dir() {
        let p = sibling(Path::new("d/clip.mp4"), ".tmp.mp4");
        assert_eq!(p, PathBuf::from("d/clip.mp4.tmp.mp4"));
    }
}
