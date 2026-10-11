//! On-disk layer for replay-buffer segments.
//!
//! Segment bytes are opaque here; no muxing happens in this module. A segment
//! is written to `seg-NNNNNN.mp4.part` and atomically renamed to
//! `seg-NNNNNN.mp4` on [`SegmentWriter::commit`]. Leftover `.part` files from a
//! crashed run are reported by [`SegmentDir::scan_for_recovery`].

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::index::SegmentMeta;

const PREFIX: &str = "seg-";
const EXT: &str = ".mp4";
const PART_EXT: &str = ".mp4.part";

/// Result of scanning a segment directory for leftovers from a previous run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecoveryReport {
    /// Committed segment files (`seg-N.mp4`), sorted by seq.
    pub complete: Vec<PathBuf>,
    /// Unfinished segment files (`seg-N.mp4.part`), sorted by seq.
    pub partial: Vec<PathBuf>,
}

/// A directory holding segment files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentDir {
    root: PathBuf,
}

/// Writer for one in-progress segment. Bytes go to the `.part` file until
/// [`SegmentWriter::commit`] renames it into place.
///
/// Dropping the writer without committing leaves the `.part` file on disk so
/// that recovery can salvage it. Use [`SegmentWriter::abort`] to remove it.
#[derive(Debug)]
pub struct SegmentWriter {
    seq: u64,
    part_path: PathBuf,
    final_path: PathBuf,
    file: BufWriter<File>,
    bytes: u64,
}

impl SegmentDir {
    /// Creates a handle for the segment directory at `root`. Does not touch disk.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The directory this handle manages.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Final (committed) path for segment `seq`, e.g. `seg-000123.mp4`.
    pub fn segment_path(&self, seq: u64) -> PathBuf {
        self.root.join(format!("{PREFIX}{seq:06}{EXT}"))
    }

    /// Opens `seg-N.mp4.part` for writing, creating the directory if needed.
    /// An existing `.part` file for the same seq is truncated.
    pub fn open_segment(&self, seq: u64) -> io::Result<SegmentWriter> {
        fs::create_dir_all(&self.root)?;
        let final_path = self.segment_path(seq);
        let mut part = final_path.clone().into_os_string();
        part.push(".part");
        let part_path = PathBuf::from(part);
        let file = File::create(&part_path)?;
        Ok(SegmentWriter {
            seq,
            part_path,
            final_path,
            file: BufWriter::new(file),
            bytes: 0,
        })
    }

    /// Deletes the backing files of evicted segments.
    ///
    /// The caller must pass only segments the index has evicted (never leased
    /// ones). Missing files are ignored. Every entry is attempted; the first
    /// real I/O error is returned after the loop. On success returns the number
    /// of files actually removed.
    pub fn delete_evicted(&self, evicted: &[SegmentMeta]) -> io::Result<usize> {
        remove_all_attempted(evicted.iter().map(|m| m.path.as_path()))
    }

    /// Lists segment files in the directory. A missing directory yields an
    /// empty report. Files that do not match the segment naming are ignored.
    pub fn scan_for_recovery(&self) -> io::Result<RecoveryReport> {
        let mut complete: Vec<(u64, PathBuf)> = Vec::new();
        let mut partial: Vec<(u64, PathBuf)> = Vec::new();
        for (seq, is_part, path) in self.list_segment_files()? {
            if is_part {
                partial.push((seq, path));
            } else {
                complete.push((seq, path));
            }
        }
        complete.sort_by_key(|(seq, _)| *seq);
        partial.sort_by_key(|(seq, _)| *seq);
        Ok(RecoveryReport {
            complete: complete.into_iter().map(|(_, p)| p).collect(),
            partial: partial.into_iter().map(|(_, p)| p).collect(),
        })
    }

    /// Collects `(seq, is_partial, path)` for every segment-named regular file.
    fn list_segment_files(&self) -> io::Result<Vec<(u64, bool, PathBuf)>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
            if !is_file {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some((seq, is_part)) = parse_segment_name(name) {
                out.push((seq, is_part, entry.path()));
            }
        }
        Ok(out)
    }
}

impl SegmentWriter {
    /// Sequence number of the segment being written.
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Path of the `.part` file currently being written.
    pub fn part_path(&self) -> &Path {
        &self.part_path
    }

    /// Bytes written so far. A convenience used by tests and tools; the
    /// recorder does not depend on it.
    #[doc(hidden)]
    pub fn bytes_written(&self) -> u64 {
        self.bytes
    }

    /// Flushes and syncs the `.part` file, then atomically renames it to the
    /// final `seg-N.mp4` name. Returns the committed size in bytes.
    pub fn commit(self) -> io::Result<u64> {
        let file = self.file.into_inner().map_err(|e| e.into_error())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&self.part_path, &self.final_path)?;
        Ok(self.bytes)
    }

    /// Discards the segment: closes the file and removes the `.part` file.
    pub fn abort(self) -> io::Result<()> {
        let SegmentWriter {
            file, part_path, ..
        } = self;
        drop(file);
        match fs::remove_file(&part_path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

impl Write for SegmentWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.file.write(buf)?;
        self.bytes += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// Parses `seg-<digits>.mp4` (returns `false`) or `seg-<digits>.mp4.part`
/// (returns `true`). Returns `None` for any other name.
fn parse_segment_name(name: &str) -> Option<(u64, bool)> {
    let rest = name.strip_prefix(PREFIX)?;
    if let Some(digits) = rest.strip_suffix(PART_EXT) {
        return parse_digits(digits).map(|seq| (seq, true));
    }
    let digits = rest.strip_suffix(EXT)?;
    parse_digits(digits).map(|seq| (seq, false))
}

/// Removes each path, ignoring files that are already gone. Every path is
/// attempted even after an error. Returns the count removed, or the first real
/// error once all paths have been tried.
fn remove_all_attempted<'a>(paths: impl Iterator<Item = &'a Path>) -> io::Result<usize> {
    let mut removed = 0usize;
    let mut first_err: Option<io::Error> = None;
    for path in paths {
        match fs::remove_file(path) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(removed),
    }
}

fn parse_digits(s: &str) -> Option<u64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}
