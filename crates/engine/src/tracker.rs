//! Segment directory tracker feeding RingIndex (task E2).
//!
//! The encoder writes `seg-000000.mp4, seg-000001.mp4, ...` into a
//! directory, 2 s each. A segment is complete once its successor exists, or,
//! for the final segment, once [`SegmentTracker::finish`] has been called. A
//! completed segment is probed for its true duration, pushed and committed
//! into the shared [`RingIndex`], and the ring is trimmed to the buffer length.
//! Evicted files are deleted from disk.
//!
//! A segment's duration comes from one of two sources:
//!
//! - The encoder's segment list (`segments.csv`, see
//!   [`clipcheck_encode::SEGMENT_LIST_FILE`]). The encoder appends one row per closed
//!   segment (`seg-NNNNNN.mp4,start,end`), so the duration is known without
//!   probing. Once the list has been seen, a segment waits for its row.
//! - An in-process MP4 duration probe, used only when no list was ever written,
//!   or after the stream has finished and a segment's row is missing (for
//!   example because the encoder stopped early). The probe runs on a worker thread owned by the
//!   tracker. [`SegmentTracker::poll_nowait`] never waits for it, so the poll
//!   thread keeps watching the directory while a probe runs.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use clipcheck_buffer::{RingIndex, SegmentDir, SegmentMeta};
use clipcheck_core::media::TICKS_PER_SEC;
use clipcheck_encode::SEGMENT_LIST_FILE;

/// Consecutive failed probes of one segment before it is quarantined. Earlier
/// failures are retried on the next poll.
const MAX_PROBE_ATTEMPTS: u32 = 3;
/// Failed deletions of one evicted file before it is abandoned. The file stays
/// on disk after that; the error is logged when it happens.
const MAX_DELETE_ATTEMPTS: u32 = 5;
/// Nominal encoder segment length, used as the timeline span of a quarantined
/// segment so later segments keep their timestamps.
const NOMINAL_SEGMENT_100NS: i64 = 2 * TICKS_PER_SEC;

/// Signature of the duration probe: `(file) -> seconds`.
type ProbeFn = fn(&Path) -> io::Result<f64>;

/// A segment the tracker has finished and indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletedSegment {
    /// Sequence number parsed from the file name.
    pub seq: u64,
    /// Path of the completed segment file.
    pub path: PathBuf,
    /// Start time, 100 ns ticks. Equals the previous segment's end.
    pub start_100ns: i64,
    /// End time, 100 ns ticks, from the probed duration.
    pub end_100ns: i64,
    /// Size of the segment file in bytes.
    pub bytes: u64,
}

/// A segment that could not be probed after [`MAX_PROBE_ATTEMPTS`] tries. It is
/// left out of the index, its file is deleted at the next flush, and a timeline
/// gap is recorded so clips never span it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedSegment {
    /// Sequence number of the skipped segment.
    pub seq: u64,
    /// Path of the skipped segment file.
    pub path: PathBuf,
    /// Last probe error.
    pub reason: String,
}

/// Outcome of trying to advance past the segment at `next_seq`.
enum Step {
    /// The segment is not ready yet, or its probe is still running.
    Waiting,
    /// The segment was probed, indexed and committed.
    Done(CompletedSegment),
    /// The segment was quarantined and the sequence advanced.
    Skipped,
}

/// A duration probe handed to the worker thread.
struct ProbeJob {
    probe: ProbeFn,
    path: PathBuf,
    seq: u64,
}

/// The worker thread's answer to one [`ProbeJob`].
struct ProbeDone {
    seq: u64,
    result: io::Result<f64>,
}

/// The segment whose probe is running on the worker thread.
#[derive(Debug)]
struct InFlight {
    seq: u64,
    path: PathBuf,
    bytes: u64,
}

/// An evicted segment whose file could not be deleted yet.
#[derive(Debug)]
struct PendingEvict {
    meta: SegmentMeta,
    /// Failed deletion attempts so far.
    attempts: u32,
}

/// Follows a segment directory and feeds completed segments into a RingIndex.
#[derive(Debug)]
pub struct SegmentTracker {
    dir: SegmentDir,
    max_duration_100ns: i64,
    shared: Arc<Mutex<RingIndex>>,
    started: Instant,
    next_seq: u64,
    last_end_100ns: i64,
    finished: bool,
    /// Evicted segments whose files still need deleting. Retried on later polls.
    pending_evict: Vec<PendingEvict>,
    /// Consecutive probe failures for the segment at `next_seq`.
    probe_failures: u32,
    /// Segments skipped because they could not be read.
    quarantined: Vec<QuarantinedSegment>,
    /// Duration probe. Tests replace it.
    probe: ProbeFn,
    /// Sends probe jobs to the worker thread. The worker exits when this drops.
    probe_tx: Sender<ProbeJob>,
    /// Receives probe results from the worker thread.
    probe_rx: Receiver<ProbeDone>,
    /// The probe currently running. Segments are probed one at a time, since
    /// the timeline is sequential.
    in_flight: Option<InFlight>,
    /// Bytes of `segments.csv` consumed so far.
    list_offset: u64,
    /// Text read from `segments.csv` that does not yet end in a newline.
    list_partial: String,
    /// Durations in seconds from `segments.csv`, by seq, not yet committed.
    list_durations: BTreeMap<u64, f64>,
    /// Seqs whose `segments.csv` row is malformed.
    list_bad: BTreeSet<u64>,
    /// `segments.csv` existed at some poll.
    list_seen: bool,
}

/// Starts the probe worker thread. Returns its job sender and result receiver.
fn spawn_probe_worker() -> (Sender<ProbeJob>, Receiver<ProbeDone>) {
    let (job_tx, job_rx) = mpsc::channel::<ProbeJob>();
    let (done_tx, done_rx) = mpsc::channel::<ProbeDone>();
    thread::spawn(move || {
        // Ends when the tracker drops its job sender.
        for job in job_rx {
            let result = (job.probe)(&job.path);
            if done_tx
                .send(ProbeDone {
                    seq: job.seq,
                    result,
                })
                .is_err()
            {
                break;
            }
        }
    });
    (job_tx, done_rx)
}

/// Error for a probe worker that stopped (its thread panicked).
fn worker_stopped() -> io::Error {
    io::Error::other("segment probe worker stopped")
}

impl SegmentTracker {
    /// Creates a tracker for the segment directory `dir`. Segments are numbered
    /// from 0. The in-process probe measures each segment's duration when the
    /// encoder's segment list has no row for it. The ring
    /// keeps at most `max_buffer_secs` of footage (plus one segment of slack,
    /// see [`RingIndex::evict_to`]). Negative or NaN values count as zero.
    pub fn new(
        dir: impl Into<PathBuf>,
        max_buffer_secs: f64,
        shared: Arc<Mutex<RingIndex>>,
    ) -> Self {
        // `f64::max` maps NaN to 0.0, and the float-to-int cast saturates.
        let secs = max_buffer_secs.max(0.0);
        let (probe_tx, probe_rx) = spawn_probe_worker();
        Self {
            dir: SegmentDir::new(dir),
            max_duration_100ns: (secs * TICKS_PER_SEC as f64) as i64,
            shared,
            started: Instant::now(),
            next_seq: 0,
            last_end_100ns: 0,
            finished: false,
            pending_evict: Vec::new(),
            probe_failures: 0,
            quarantined: Vec::new(),
            probe: probe_duration,
            probe_tx,
            probe_rx,
            in_flight: None,
            list_offset: 0,
            list_partial: String::new(),
            list_durations: BTreeMap::new(),
            list_bad: BTreeSet::new(),
            list_seen: false,
        }
    }

    /// Segments skipped so far because they could not be read.
    pub fn quarantined(&self) -> &[QuarantinedSegment] {
        &self.quarantined
    }

    /// Marks the final segment as complete. The next [`poll`](Self::poll)
    /// reports it if its file exists. Call this after the encoder has exited.
    pub fn finish(&mut self) {
        self.finished = true;
    }

    /// Time since the tracker was created, in 100 ns ticks.
    pub fn now_100ns(&self) -> i64 {
        (self.started.elapsed().as_nanos() / 100).min(i64::MAX as u128) as i64
    }

    /// Completes every segment that is ready, in sequence order. Waits for each
    /// segment's probe to finish. Use [`poll_nowait`](Self::poll_nowait) on a
    /// thread that must keep polling the directory.
    ///
    /// Each returned segment has been pushed and committed into the shared
    /// index, and older segments beyond the buffer length have been evicted
    /// and deleted. If a segment fails to complete after earlier ones
    /// succeeded, the successes are returned, the failure is logged, and it is
    /// retried on the next poll. An error is returned only when nothing completed.
    pub fn poll(&mut self) -> io::Result<Vec<CompletedSegment>> {
        self.poll_with(true)
    }

    /// Like [`poll`](Self::poll), but never waits for the duration probe. A segment whose
    /// probe is still running completes on a later call. The poll thread uses
    /// this so a slow probe does not hold up directory polling.
    pub fn poll_nowait(&mut self) -> io::Result<Vec<CompletedSegment>> {
        self.poll_with(false)
    }

    fn poll_with(&mut self, wait: bool) -> io::Result<Vec<CompletedSegment>> {
        let mut out = Vec::new();
        let result = self.run(wait, &mut |seg| out.push(seg));
        match result {
            Ok(()) => Ok(out),
            // The error is returned only when nothing completed. Otherwise the
            // completed segments are returned and the failure is retried on the
            // next poll, so it is logged here rather than dropped.
            Err(e) if out.is_empty() => Err(e),
            Err(e) => {
                eprintln!("clipcheck: segment tracker: {e} (will retry)");
                Ok(out)
            }
        }
    }

    /// Completes segments until the next one is not ready, passing each to
    /// `emit`. Always deletes queued evicted files afterwards.
    fn run(&mut self, wait: bool, emit: &mut dyn FnMut(CompletedSegment)) -> io::Result<()> {
        let advanced = self.advance_all(wait, emit);
        self.flush_evicted();
        advanced
    }

    fn advance_all(
        &mut self,
        wait: bool,
        emit: &mut dyn FnMut(CompletedSegment),
    ) -> io::Result<()> {
        loop {
            match self.advance(wait)? {
                Step::Done(seg) => emit(seg),
                Step::Skipped => {}
                Step::Waiting => return Ok(()),
            }
        }
    }

    /// Runs the tracker to the end of the stream: waits for every probe and
    /// retries failed ones until each segment is completed or quarantined.
    /// Used once the encoder has stopped. Returns an error only when the same
    /// failure repeats without progress.
    fn settle(&mut self, emit: &mut dyn FnMut(CompletedSegment)) -> io::Result<()> {
        let mut failures = 0u32;
        loop {
            let before = self.next_seq;
            match self.run(true, emit) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    failures = if self.next_seq == before {
                        failures + 1
                    } else {
                        1
                    };
                    if failures >= MAX_PROBE_ATTEMPTS {
                        return Err(e);
                    }
                    eprintln!("clipcheck: segment tracker: {e} (will retry)");
                }
            }
        }
    }

    /// Advances past the segment at `next_seq`. Starts its probe when it is
    /// ready. With `wait`, blocks until that probe finishes; otherwise a probe
    /// still running reports [`Step::Waiting`].
    ///
    /// With no probe in flight, the segment list is read first and the
    /// duration is taken from it when the row exists. A malformed row counts
    /// as a failed probe. Without a list row, the list has been seen and the
    /// stream is still running, so the segment waits. Otherwise the in-process
    /// probe fallback runs.
    fn advance(&mut self, wait: bool) -> io::Result<Step> {
        if self.in_flight.is_none() {
            self.read_segment_list()?;
            let seq = self.next_seq;
            let path = self.dir.segment_path(seq);

            if let Some(duration_secs) = self.list_durations.remove(&seq) {
                let Some(bytes) = file_len(&path)? else {
                    self.list_durations.insert(seq, duration_secs);
                    return Ok(Step::Waiting);
                };
                self.probe_failures = 0;
                return self.commit(seq, path, bytes, duration_secs).map(Step::Done);
            }
            if self.list_bad.contains(&seq) {
                let Some(bytes) = file_len(&path)? else {
                    return Ok(Step::Waiting);
                };
                self.probe_failures += 1;
                let err = bad_row_error(seq);
                if self.probe_failures < MAX_PROBE_ATTEMPTS {
                    return Err(err);
                }
                self.list_bad.remove(&seq);
                self.quarantine(seq, path, bytes, &err);
                return Ok(Step::Skipped);
            }
            if self.list_seen && !self.finished {
                return Ok(Step::Waiting);
            }

            let Some(bytes) = file_len(&path)? else {
                return Ok(Step::Waiting);
            };
            // Once finished, the successor's existence is irrelevant, so skip its stat.
            if !self.finished && file_len(&self.dir.segment_path(seq + 1))?.is_none() {
                return Ok(Step::Waiting);
            }
            let job = ProbeJob {
                probe: self.probe,
                path: path.clone(),
                seq,
            };
            self.probe_tx.send(job).map_err(|_| worker_stopped())?;
            self.in_flight = Some(InFlight { seq, path, bytes });
        }

        let done = if wait {
            self.probe_rx.recv().map_err(|_| worker_stopped())?
        } else {
            match self.probe_rx.try_recv() {
                Ok(done) => done,
                Err(TryRecvError::Empty) => return Ok(Step::Waiting),
                Err(TryRecvError::Disconnected) => return Err(worker_stopped()),
            }
        };
        let flight = self
            .in_flight
            .take()
            .ok_or_else(|| io::Error::other("probe result without a probe in flight"))?;
        if done.seq != flight.seq {
            return Err(io::Error::other(format!(
                "probe result for segment {} while waiting for {}",
                done.seq, flight.seq
            )));
        }
        let InFlight { seq, path, bytes } = flight;

        let duration_secs = match done.result {
            Ok(secs) => {
                self.probe_failures = 0;
                secs
            }
            Err(e) => {
                self.probe_failures += 1;
                if self.probe_failures < MAX_PROBE_ATTEMPTS {
                    return Err(e);
                }
                self.quarantine(seq, path, bytes, &e);
                return Ok(Step::Skipped);
            }
        };
        self.commit(seq, path, bytes, duration_secs).map(Step::Done)
    }

    /// Pushes and commits a probed segment into the shared index, and returns it.
    fn commit(
        &mut self,
        seq: u64,
        path: PathBuf,
        bytes: u64,
        duration_secs: f64,
    ) -> io::Result<CompletedSegment> {
        let start = self.last_end_100ns;
        let duration = (duration_secs * TICKS_PER_SEC as f64).round() as i64;
        let end = start.saturating_add(duration);

        let evicted = {
            let mut idx = self
                .shared
                .lock()
                .map_err(|_| io::Error::other("ring index lock poisoned"))?;
            idx.push_committed(
                SegmentMeta {
                    seq,
                    start_100ns: start,
                    end_100ns: end,
                    // Each encoder segment begins with a keyframe.
                    first_keyframe_100ns: start,
                    bytes,
                    path: path.clone(),
                    committed: false,
                },
                end,
                bytes,
            )
            .map_err(io::Error::other)?;
            idx.evict_to(self.max_duration_100ns, u64::MAX)
        };

        // State advances before any fallible file deletion, so the segment is
        // never re-pushed. Failed deletions go to `pending_evict`.
        self.queue_evicted(evicted);
        self.last_end_100ns = end;
        self.next_seq = seq + 1;

        Ok(CompletedSegment {
            seq,
            path,
            start_100ns: start,
            end_100ns: end,
            bytes,
        })
    }

    /// Queues evicted segments for file deletion.
    fn queue_evicted(&mut self, evicted: Vec<SegmentMeta>) {
        self.pending_evict.extend(
            evicted
                .into_iter()
                .map(|meta| PendingEvict { meta, attempts: 0 }),
        );
    }

    /// Skips the unreadable segment at `next_seq`. The timeline advances by the
    /// nominal segment length, a gap is recorded at the end of that span so
    /// clips do not reach across it, and the file is queued for deletion.
    fn quarantine(&mut self, seq: u64, path: PathBuf, bytes: u64, err: &io::Error) {
        let start = self.last_end_100ns;
        let end = start.saturating_add(NOMINAL_SEGMENT_100NS);
        if let Ok(mut idx) = self.shared.lock() {
            idx.mark_gap(end);
        }
        self.pending_evict.push(PendingEvict {
            meta: SegmentMeta {
                seq,
                start_100ns: start,
                end_100ns: end,
                first_keyframe_100ns: start,
                bytes,
                path: path.clone(),
                committed: true,
            },
            attempts: 0,
        });
        self.quarantined.push(QuarantinedSegment {
            seq,
            path: path.clone(),
            reason: err.to_string(),
        });
        self.last_end_100ns = end;
        self.next_seq = seq + 1;
        self.probe_failures = 0;
        eprintln!(
            "clipcheck: skipped unreadable segment {seq} ({}): {err}",
            path.display()
        );
    }

    /// Deletes the files of evicted segments, one file at a time, so a file
    /// that cannot be removed (for example one still held open by a player)
    /// does not block the others. A failed file is kept for a later call. After
    /// [`MAX_DELETE_ATTEMPTS`] failures it is dropped and the error logged, so the
    /// queue cannot grow without bound. The file then stays on disk.
    fn flush_evicted(&mut self) {
        for mut entry in std::mem::take(&mut self.pending_evict) {
            match self.dir.delete_evicted(std::slice::from_ref(&entry.meta)) {
                Ok(_) => {}
                Err(e) => {
                    entry.attempts += 1;
                    let path = entry.meta.path.display();
                    if entry.attempts >= MAX_DELETE_ATTEMPTS {
                        eprintln!(
                            "clipcheck: giving up deleting evicted segment {path} after {} attempts: {e}",
                            entry.attempts
                        );
                    } else {
                        if entry.attempts == 1 {
                            eprintln!(
                                "clipcheck: cannot delete evicted segment {path} (will retry): {e}"
                            );
                        }
                        self.pending_evict.push(entry);
                    }
                }
            }
        }
    }
}

/// Runs a [`SegmentTracker`] on a background thread.
#[derive(Debug)]
pub struct TrackerThread {
    stop_tx: mpsc::Sender<()>,
    handle: JoinHandle<io::Result<()>>,
}

impl TrackerThread {
    /// Starts polling `tracker` every `poll_every`. Each completed segment is
    /// passed to `on_complete` in order. This is the hook the session
    /// assembler uses.
    pub fn spawn(
        mut tracker: SegmentTracker,
        poll_every: Duration,
        on_complete: Box<dyn Fn(&CompletedSegment) + Send>,
    ) -> TrackerThread {
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            // Transient errors are retried on the next tick. Each one is
            // reported here, since no caller can receive it mid-run.
            while let Err(RecvTimeoutError::Timeout) = stop_rx.recv_timeout(poll_every) {
                if let Err(e) = drain(&mut tracker, &*on_complete) {
                    eprintln!("clipcheck: segment tracker: {e} (will retry)");
                }
            }
            tracker.finish();
            // Waits for the last probes and retries failed ones, so every
            // remaining segment reaches `on_complete` or the quarantine list.
            let mut emit = |seg: CompletedSegment| on_complete(&seg);
            tracker.settle(&mut emit)?;
            let skipped = tracker.quarantined();
            if skipped.is_empty() {
                return Ok(());
            }
            let seqs: Vec<String> = skipped.iter().map(|q| q.seq.to_string()).collect();
            Err(io::Error::other(format!(
                "{} segment(s) could not be read and were skipped (seq {})",
                skipped.len(),
                seqs.join(", ")
            )))
        });
        TrackerThread { stop_tx, handle }
    }

    /// Stops the thread. The final pass marks the last segment complete and
    /// polls once more, so every remaining segment reaches `on_complete`.
    /// Returns an error if that pass failed, if the thread panicked, or if any
    /// segment was skipped as unreadable during the run.
    pub fn stop(self) -> io::Result<()> {
        let TrackerThread { stop_tx, handle } = self;
        drop(stop_tx);
        handle
            .join()
            .map_err(|_| io::Error::other("segment tracker thread panicked"))?
    }
}

/// Polls once without waiting for probes and passes each completed segment to
/// `on_complete`.
fn drain(tracker: &mut SegmentTracker, on_complete: &dyn Fn(&CompletedSegment)) -> io::Result<()> {
    for seg in tracker.poll_nowait()? {
        on_complete(&seg);
    }
    Ok(())
}

impl SegmentTracker {
    /// Reads the rows appended to `segments.csv` since the last call. Only
    /// complete lines are consumed, so a row still being written is picked up
    /// whole on a later call. A missing file is not an error.
    fn read_segment_list(&mut self) -> io::Result<()> {
        let path = self.dir.root().join(SEGMENT_LIST_FILE);
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        self.list_seen = true;
        // A file that shrank was replaced. Read it again from the start.
        if file.metadata()?.len() < self.list_offset {
            self.list_offset = 0;
            self.list_partial.clear();
        }
        file.seek(SeekFrom::Start(self.list_offset))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        self.list_offset += buf.len() as u64;
        self.list_partial.push_str(&String::from_utf8_lossy(&buf));

        while let Some(newline) = self.list_partial.find('\n') {
            let line: String = self.list_partial.drain(..=newline).collect();
            match parse_list_row(&line) {
                ListRow::Duration { seq, secs } if seq >= self.next_seq => {
                    self.list_durations.insert(seq, secs);
                }
                ListRow::Malformed { seq } if seq >= self.next_seq => {
                    self.list_bad.insert(seq);
                }
                ListRow::Duration { .. } | ListRow::Malformed { .. } | ListRow::Skip => {}
            }
        }
        Ok(())
    }
}

/// One parsed line of `segments.csv`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ListRow {
    /// A usable row: segment `seq` lasts `secs` seconds.
    Duration { seq: u64, secs: f64 },
    /// A row for segment `seq` whose times are not usable.
    Malformed { seq: u64 },
    /// An empty line, or a row whose file name is not a segment name.
    Skip,
}

/// Parses one `segments.csv` line: `seg-NNNNNN.mp4,start_secs,end_secs`, with
/// the times on the encoder's timeline. Surrounding whitespace, including a
/// trailing `\r`, is ignored.
fn parse_list_row(line: &str) -> ListRow {
    let line = line.trim();
    if line.is_empty() {
        return ListRow::Skip;
    }
    // Split from the right, so the file name is whatever comes first.
    let mut fields = line.rsplitn(3, ',');
    let (Some(end), Some(start), Some(name)) = (fields.next(), fields.next(), fields.next()) else {
        return ListRow::Skip;
    };
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let Some(seq) = parse_segment_file_name(base.trim()) else {
        return ListRow::Skip;
    };
    let times = (start.trim().parse::<f64>(), end.trim().parse::<f64>());
    match times {
        (Ok(start), Ok(end)) if start.is_finite() && end.is_finite() && end >= start => {
            ListRow::Duration {
                seq,
                secs: end - start,
            }
        }
        _ => ListRow::Malformed { seq },
    }
}

/// Parses a segment file name `seg-<digits>.mp4` into its sequence number.
fn parse_segment_file_name(name: &str) -> Option<u64> {
    let digits = name.strip_prefix("seg-")?.strip_suffix(".mp4")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// The error reported for a segment whose `segments.csv` row is malformed.
fn bad_row_error(seq: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("malformed {SEGMENT_LIST_FILE} row for segment {seq}"),
    )
}

/// Size of `path` in bytes, or `None` when it does not exist.
fn file_len(path: &Path) -> io::Result<Option<u64>> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(Some(meta.len())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Probes the duration of `file` in seconds, read in-process with the MP4 reader.
fn probe_duration(file: &Path) -> io::Result<f64> {
    let movie = clipcheck_mp4::reader::read_movie(file).map_err(|e| match e {
        clipcheck_mp4::Mp4Error::Io(io_err) => io_err,
        other => io::Error::new(
            io::ErrorKind::InvalidData,
            format!("cannot read duration of {}: {other}", file.display()),
        ),
    })?;
    let secs = movie.duration_secs;
    if !secs.is_finite() || secs < 0.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid duration {secs} for {}", file.display()),
        ));
    }
    Ok(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clipcheck_buffer::SegmentDir;
    use std::sync::Mutex as StdMutex;

    /// Fake probe: a file that starts with `GARBAGE` is unreadable, any other
    /// file is 2 seconds long. Nothing is executed.
    fn fake_probe(file: &Path) -> io::Result<f64> {
        if std::fs::read(file)?.starts_with(b"GARBAGE") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "garbage segment",
            ));
        }
        Ok(2.0)
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("clipcheck-tracker-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create dir");
        dir
    }

    /// Writes segments 0..=3 where seq 1 is unreadable.
    fn write_segments(dir: &Path) -> SegmentDir {
        let seg = SegmentDir::new(dir);
        let segments: [(u64, &[u8]); 4] = [(0, b"ok"), (1, b"GARBAGE"), (2, b"ok"), (3, b"ok")];
        for (seq, body) in segments {
            std::fs::write(seg.segment_path(seq), body).expect("write segment");
        }
        seg
    }

    fn new_tracker(dir: &Path, shared: Arc<Mutex<RingIndex>>) -> SegmentTracker {
        let mut tracker = SegmentTracker::new(dir, 1000.0, shared);
        tracker.probe = fake_probe;
        tracker
    }

    #[test]
    fn unreadable_segment_is_quarantined_and_tracking_continues() {
        let dir = scratch_dir("quarantine");
        let seg = write_segments(&dir);
        let shared = Arc::new(Mutex::new(RingIndex::new()));
        let mut tracker = new_tracker(&dir, shared.clone());
        tracker.finish();

        let mut done = Vec::new();
        let mut errors = 0;
        for _ in 0..6 {
            match tracker.poll() {
                Ok(segs) => done.extend(segs.into_iter().map(|s| s.seq)),
                Err(_) => errors += 1,
            }
        }

        assert_eq!(
            done,
            vec![0, 2, 3],
            "segments after the bad one still complete"
        );
        assert_eq!(errors, 1, "only the second failed attempt returns an error");
        let q = tracker.quarantined();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].seq, 1);
        assert!(q[0].reason.contains("garbage"), "reason: {}", q[0].reason);
        assert!(!seg.segment_path(1).exists(), "quarantined file is deleted");

        let idx = shared.lock().expect("lock");
        let seqs: Vec<u64> = idx.iter().map(|s| s.seq).collect();
        assert_eq!(seqs, vec![0, 2, 3]);
        // Segment 0 spans 0..2 s, the skipped segment 2..4 s.
        assert_eq!(idx.gap_100ns(), Some(2 * NOMINAL_SEGMENT_100NS));
        drop(idx);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transient_failure_is_retried_without_quarantine() {
        let dir = scratch_dir("transient");
        let seg = SegmentDir::new(&dir);
        std::fs::write(seg.segment_path(0), b"GARBAGE").expect("write");
        std::fs::write(seg.segment_path(1), b"ok").expect("write");
        let shared = Arc::new(Mutex::new(RingIndex::new()));
        let mut tracker = new_tracker(&dir, shared);
        // Heal the segment before the probe limit is reached.
        tracker.finish();
        assert!(tracker.poll().is_err());
        std::fs::write(seg.segment_path(0), b"ok").expect("heal");
        let segs = tracker.poll().expect("retry succeeds");
        assert_eq!(segs.iter().map(|s| s.seq).collect::<Vec<_>>(), vec![0, 1]);
        assert!(tracker.quarantined().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn thread_reports_skipped_segments_on_stop() {
        let dir = scratch_dir("thread");
        write_segments(&dir);
        let shared = Arc::new(Mutex::new(RingIndex::new()));
        let tracker = new_tracker(&dir, shared);
        let completed = Arc::new(StdMutex::new(Vec::new()));
        let sink = completed.clone();
        let thread = TrackerThread::spawn(
            tracker,
            Duration::from_millis(10),
            Box::new(move |seg: &CompletedSegment| {
                sink.lock().expect("lock").push(seg.seq);
            }),
        );
        let result = thread.stop();
        let err = result.expect_err("stop reports the skipped segment");
        assert!(err.to_string().contains("seq 1"), "message: {err}");
        assert_eq!(*completed.lock().expect("lock"), vec![0, 2, 3]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn duration_of(line: &str) -> (u64, f64) {
        match parse_list_row(line) {
            ListRow::Duration { seq, secs } => (seq, secs),
            other => panic!("expected a duration for {line:?}, got {other:?}"),
        }
    }

    #[test]
    fn list_row_valid_and_crlf_give_duration() {
        let (seq, secs) = duration_of("seg-000002.mp4,4.000000,6.033333\n");
        assert_eq!(seq, 2);
        assert!((secs - 2.033333).abs() < 1e-9, "secs {secs}");
        // CRLF line endings and a directory prefix are tolerated.
        assert_eq!(
            duration_of("seg-000000.mp4,0.000000,2.000000\r\n"),
            (0, 2.0)
        );
        assert_eq!(duration_of("out\\seg-000007.mp4,1,3\r\n"), (7, 2.0));
    }

    #[test]
    fn list_row_bad_numbers_and_order_are_malformed() {
        assert_eq!(
            parse_list_row("seg-000001.mp4,abc,2.0\n"),
            ListRow::Malformed { seq: 1 }
        );
        assert_eq!(
            parse_list_row("seg-000001.mp4,0,NaN\n"),
            ListRow::Malformed { seq: 1 }
        );
        assert_eq!(
            parse_list_row("seg-000003.mp4,5.0,4.0\n"),
            ListRow::Malformed { seq: 3 }
        );
        assert_eq!(parse_list_row("seg-000004.mp4,0\n"), ListRow::Skip);
    }

    #[test]
    fn list_row_wrong_name_and_blank_are_skipped() {
        assert_eq!(parse_list_row("other.mp4,0.0,2.0\n"), ListRow::Skip);
        assert_eq!(parse_list_row("seg-000001.mov,0.0,2.0\n"), ListRow::Skip);
        assert_eq!(parse_list_row("seg-1x.mp4,0.0,2.0\n"), ListRow::Skip);
        assert_eq!(parse_list_row("\r\n"), ListRow::Skip);
    }

    #[test]
    fn segment_list_supplies_durations_without_probing() {
        fn never_probe(file: &Path) -> io::Result<f64> {
            Err(io::Error::other(format!(
                "probe must not run for {}",
                file.display()
            )))
        }
        let dir = scratch_dir("list");
        let seg = SegmentDir::new(&dir);
        std::fs::write(seg.segment_path(0), b"ok").expect("write");
        std::fs::write(seg.segment_path(1), b"ok").expect("write");
        // Row for segment 1 is still being written: no newline yet.
        std::fs::write(
            dir.join(SEGMENT_LIST_FILE),
            "seg-000000.mp4,0.000000,3.000000\nseg-000001.mp4,3.000000,4.500000",
        )
        .expect("write list");
        let shared = Arc::new(Mutex::new(RingIndex::new()));
        let mut tracker = new_tracker(&dir, shared);
        tracker.probe = never_probe;

        let segs = tracker.poll().expect("poll");
        assert_eq!(segs.iter().map(|s| s.seq).collect::<Vec<_>>(), vec![0]);
        assert_eq!(segs[0].end_100ns, 3 * TICKS_PER_SEC);
        // Segment 1 has no complete row yet, so it waits.
        assert!(tracker.poll().expect("poll").is_empty());

        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(SEGMENT_LIST_FILE))
            .expect("open list");
        f.write_all(b"\n").expect("finish row");
        drop(f);
        let segs = tracker.poll().expect("poll");
        assert_eq!(segs.iter().map(|s| s.seq).collect::<Vec<_>>(), vec![1]);
        assert_eq!(segs[0].start_100ns, 3 * TICKS_PER_SEC);
        assert_eq!(
            segs[0].end_100ns,
            3 * TICKS_PER_SEC + 15 * TICKS_PER_SEC / 10
        );
        assert!(tracker.quarantined().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
