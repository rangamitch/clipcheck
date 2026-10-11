//! Single-window recorder (task R1, design D14/D18/D19, D48).
//!
//! A [`Recorder`] records one attached game window at a time. Attaching starts
//! a window capture into a fresh run directory, and a [`TrackerThread`] follows
//! the completed encoder segments. Completed segments feed a shared
//! [`RingIndex`] (the replay buffer, for clips) and, while a session is active,
//! a [`SessionAssembler`] (for session parts).
//!
//! Clips are planned synchronously and extracted on a worker thread, reporting
//! through [`RecorderEvent`]. Session parts are written asynchronously (D48):
//! [`Recorder::session_split`], [`Recorder::session_stop`] and [`Recorder::detach`]
//! queue the part on one FIFO finalizer thread and return at once. Parts are
//! written in submission order, and each outcome arrives as a
//! [`RecorderEvent::SessionPartSaved`] or [`RecorderEvent::SessionPartFailed`].
//! [`Recorder::wait_idle`] waits until the queue is empty.
//!
//! Only the window capture is Windows-only. The rest of the module compiles
//! everywhere, and [`Recorder::attach`] fails with [`RecorderError::Unsupported`]
//! off Windows.

mod helpers;
pub(crate) mod naming;

use std::collections::HashSet;
use std::fs;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use clipcheck_buffer::{extract_clip, Lease, RingIndex, Window};
use clipcheck_core::media::TICKS_PER_SEC;

use clipcheck_encode::pipe::AudioTrackSpec;
use clipcheck_encode::tuning::EncoderTuning;

use crate::audiofeed::AudioFeeder;
pub use crate::audiofeed::{AudioSourceKind, AudioTrackConfig, TrackSource};
pub use crate::audiomix::LevelSnapshot;
use crate::canvas::canvas_for_source;
use crate::procs::lock;
use crate::sessionfile::SessionAssembler;
use crate::sweep::{self, SweepReport};
use crate::tracker::{SegmentTracker, TrackerThread};
use crate::window_size::window_size;
use helpers::{
    io_err, new_session, plan_err, segment_sink, start_capture, stop_capture, window_pid,
    AudioLanes,
};

#[cfg(windows)]
use crate::pipeline::RunningCapture;

/// How often the tracker polls the segment directory.
const TRACKER_POLL: Duration = Duration::from_millis(250);
/// Ring length in seconds when the replay buffer is disabled. Clips are refused
/// in that case. The ring only keeps the newest segments on disk long enough
/// for a session to hard-link them before they are evicted.
const DISABLED_RING_SECS: f64 = 8.0;
/// How often the audio watcher takes new feeder warnings during a run.
const AUDIO_WATCH_POLL: Duration = Duration::from_millis(250);
/// How often [`Recorder::wait_idle`] checks the finalizer queue.
const WAIT_IDLE_POLL: Duration = Duration::from_millis(10);

/// Encoder canvas for a recording (D54). `probe` is the captured window's size, when known.
///
/// Returns `fallback` (the configured 16:9 canvas) when the probe is missing or either
/// dimension is below 64 pixels. Otherwise the canvas height is `canvas_height`, or the
/// source height capped at 2160 when `canvas_height` is 0 (Native), and the width follows
/// the source aspect via [`canvas_for_source`].
pub(crate) fn choose_canvas(
    fallback: (u32, u32),
    canvas_height: u32,
    probe: Option<(u32, u32)>,
) -> (u32, u32) {
    let Some((probe_w, probe_h)) = probe else {
        return fallback;
    };
    if probe_w < 64 || probe_h < 64 {
        return fallback;
    }
    let height = if canvas_height != 0 {
        canvas_height
    } else {
        probe_h.min(2160)
    };
    canvas_for_source(probe_w, probe_h, height)
}

/// Handle to a running window capture. Only Windows has one.
#[cfg(windows)]
type CaptureHandle = RunningCapture;
/// Handle to a running window capture. Off Windows there is none.
#[cfg(not(windows))]
type CaptureHandle = ();

/// Stage directories of session parts that failed to write, shared by a run
/// and its finalizer jobs. They hold the only copies of those segments.
type Kept = Arc<Mutex<Vec<PathBuf>>>;

/// Static recorder settings.
#[derive(Debug, Clone, PartialEq)]
pub struct RecorderConfig {
    /// Encoder name passed to the encoder (for example `libx264` or `h264_nvenc`).
    pub encoder_name: String,
    /// Capture and output frame rate.
    pub fps: u32,
    /// Output canvas size (width, height).
    pub canvas: (u32, u32),
    /// Target canvas height for the source-aspect canvas (D54). 0 = Native (source height,
    /// capped at 2160). `canvas` is the 16:9 fallback when the window size is unknown.
    pub canvas_height: u32,
    /// Target video bitrate in kilobits per second.
    pub bitrate_kbps: u32,
    /// Whether the replay buffer keeps segments for clips.
    pub buffer_enabled: bool,
    /// Replay buffer length in seconds.
    pub buffer_max_secs: f64,
    /// Parent of the per-attach run directories. Buffer segments live in
    /// `work_dir/<run id>/` while attached.
    pub work_dir: PathBuf,
    /// Base folder for saved clips, normally `<root>\Clips`.
    pub clips_dir: PathBuf,
    /// Base folder for saved session parts, normally `<root>\Sessions`.
    pub sessions_dir: PathBuf,
    /// When true, outputs go into `<base>\<Game>\`. When false they go directly
    /// into the base folder, as before per-game folders existed.
    pub per_game_folders: bool,
    /// Whether `attach` starts a session immediately.
    pub session_auto_start: bool,
    /// Audio tracks, one per AAC track in the output, in order. Empty records
    /// video only.
    pub audio_tracks: Vec<AudioTrackConfig>,
    /// Encoder rate control, speed, B-frames and GPU index. The default
    /// (`EncoderTuning::default()`) is the behaviour before tuning existed.
    pub tuning: EncoderTuning,
}

/// Identifies a clip job started by [`Recorder::clip`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JobId(pub u64);

/// Notifications from the recorder. Delivered on the channel passed to
/// [`Recorder::new`].
#[derive(Debug, Clone, PartialEq)]
pub enum RecorderEvent {
    /// A clip was written.
    ClipSaved {
        /// The job that produced the clip.
        job: JobId,
        /// Path of the written clip.
        path: PathBuf,
        /// Length the caller asked for.
        seconds_requested: u32,
        /// Length actually planned, after keyframe snapping and buffer limits.
        seconds_actual: f64,
        /// True when the buffer held less than the requested length.
        short: bool,
    },
    /// A clip could not be written.
    ClipFailed {
        /// The job that failed.
        job: JobId,
        /// Error text from the extraction.
        error: String,
    },
    /// A session part was written.
    SessionPartSaved {
        /// Path of the written part.
        path: PathBuf,
        /// One-based part number within the session.
        part_index: u32,
        /// True for the last part of a session (stop or detach), false for a split.
        final_part: bool,
    },
    /// A session part could not be written. Its staged segments are kept in `kept_stage`.
    SessionPartFailed {
        /// One-based part number within the session.
        part_index: u32,
        /// True for the last part of a session (stop or detach), false for a split.
        final_part: bool,
        /// Error text from the write.
        error: String,
        /// Stage directory holding the part's segments, kept for recovery.
        kept_stage: PathBuf,
    },
    /// A non-fatal problem, for example a segment that could not be staged.
    Error(String),
    /// The recording continues, but something is degraded. For example an audio
    /// track fell back to silence because its source could not start.
    Warning(String),
}

/// Snapshot of the recorder state, from [`Recorder::status`].
#[derive(Debug, Clone, PartialEq)]
pub struct RecorderStatus {
    /// Label of the attached window, or `None` when idle.
    pub attached: Option<String>,
    /// True while a session is recording.
    pub session_active: bool,
    /// Part number the active session is writing. 0 when no session is active.
    pub part_index: u32,
    /// Seconds of footage held by the replay buffer. 0 when disabled or idle.
    pub buffer_secs: f64,
    /// Frames dropped by the pipeline. The live count is not exposed while
    /// attached, so this reports the count from the last detached run.
    pub frames_dropped: u64,
    /// Session part writes queued or running on the finalizer. Also counts
    /// parts still being written after a detach.
    pub finalizing: u32,
}

/// Errors from [`Recorder`] calls.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecorderError {
    /// The call needs an attached window.
    #[error("no window is attached")]
    NotAttached,
    /// `attach` was called while a window is already attached.
    #[error("a window is already attached")]
    AlreadyAttached,
    /// `session_start` was called while a session is already active.
    #[error("a session is already active")]
    SessionActive,
    /// The call needs an active session.
    #[error("no session is active")]
    NoSession,
    /// `session_split` was called before any segment was staged.
    #[error("the current session part holds no segments yet")]
    NoSegments,
    /// `clip` was called with the replay buffer disabled.
    #[error("the replay buffer is disabled")]
    BufferDisabled,
    /// The buffer holds no segments yet.
    #[error("buffer holds no segments to clip")]
    BufferEmpty,
    /// The request is invalid, for example a zero-second clip.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// Recording is only available on Windows.
    #[error("recording is not supported on this platform")]
    Unsupported,
    /// An I/O or process error, with its message.
    #[error("{0}")]
    Io(String),
}

/// Takes the warnings in `batch` that `seen` has not reported yet, and records
/// them in `seen`. Repeats of an earlier message are dropped.
fn fresh_warnings(seen: &mut HashSet<String>, batch: Vec<String>) -> Vec<String> {
    batch
        .into_iter()
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

/// Background reader that sends audio warnings as they appear, so they are
/// reported during the run and not only at stop. Stopped by dropping its sender.
struct AudioWatch {
    stop: Option<Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl AudioWatch {
    fn spawn(warnings: Arc<Mutex<Vec<String>>>, events: Sender<RecorderEvent>) -> io::Result<Self> {
        let (stop, stop_rx) = mpsc::channel::<()>();
        let handle = thread::Builder::new()
            .name("clipcheck-audio-watch".into())
            .spawn(move || {
                let mut seen = HashSet::new();
                // Any message or a disconnect ends the loop. Only a timeout polls.
                while let Err(RecvTimeoutError::Timeout) = stop_rx.recv_timeout(AUDIO_WATCH_POLL) {
                    let batch = std::mem::take(&mut *lock(&warnings));
                    for warning in fresh_warnings(&mut seen, batch) {
                        if events.send(RecorderEvent::Warning(warning)).is_err() {
                            return;
                        }
                    }
                }
            })?;
        Ok(Self {
            stop: Some(stop),
            handle: Some(handle),
        })
    }

    /// Ends the reader and waits for it to exit.
    fn stop(&mut self) {
        self.stop.take();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// State for one attached window.
struct Run {
    /// The caller's label for the window.
    label: String,
    /// Sanitized game name. Used as the file-name prefix and, when per-game
    /// folders are on, as the folder name.
    game: String,
    run_dir: PathBuf,
    /// Time origin for the replay buffer clock.
    started: Instant,
    capture: Option<CaptureHandle>,
    /// Audio capture feeding the capture's audio tracks. `None` when there are none.
    audio: Option<AudioFeeder>,
    /// Live reader of `audio`'s warnings. `None` when there is no audio.
    audio_watch: Option<AudioWatch>,
    tracker: Option<TrackerThread>,
    index: Arc<Mutex<RingIndex>>,
    session: Arc<Mutex<Option<ActiveSession>>>,
    /// Number of sessions started for this attach. Names each session's stage dirs.
    session_seq: u32,
    /// Stage directories of session parts that failed to write. Shared with the
    /// finalizer jobs, which add to it. `detach` keeps them.
    kept_stages: Kept,
}

impl Run {
    /// Wall-clock time since the run started, in 100 ns ticks.
    fn now_100ns(&self) -> i64 {
        (self.started.elapsed().as_nanos() / 100).min(i64::MAX as u128) as i64
    }
}

/// A session part being staged.
struct ActiveSession {
    asm: SessionAssembler,
    part_index: u32,
}

/// Keeps a clip's segments leased until extraction finishes. Dropping the guard
/// releases the lease, including when the worker thread never starts.
struct LeaseGuard {
    index: Arc<Mutex<RingIndex>>,
    lease: Option<Lease>,
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        if let Some(lease) = self.lease.take() {
            lock(&self.index).release(lease);
        }
    }
}

/// Work for the finalizer thread. Jobs run in submission order.
enum Job {
    /// Write one session part (or skip it when it holds no segments).
    Part {
        game: String,
        active: ActiveSession,
        final_part: bool,
        kept: Kept,
    },
    /// Wait for the run's clip workers, then delete its run directory except
    /// the kept stages.
    Cleanup {
        run_dir: PathBuf,
        kept: Kept,
        workers: Vec<JoinHandle<()>>,
    },
}

/// State the finalizer jobs need. Shared by the finalizer thread and by inline
/// execution when the thread is unavailable.
struct Ctx {
    cfg: RecorderConfig,
    events: Sender<RecorderEvent>,
}

impl Ctx {
    /// Sends an event. Ignores a disconnected receiver.
    fn emit(&self, event: RecorderEvent) {
        let _ = self.events.send(event);
    }

    /// Writes one session part. A part with no segments is skipped. On success
    /// emits [`RecorderEvent::SessionPartSaved`]. On failure the stage directory
    /// is pushed onto `kept` (its segments are then the only copies), and
    /// [`RecorderEvent::SessionPartFailed`] and an `Error` are emitted.
    fn run_part(&self, game: &str, mut active: ActiveSession, final_part: bool, kept: &Kept) {
        if active.asm.segment_count() == 0 {
            return;
        }
        let stage = active.asm.stage_dir().to_path_buf();
        let part_index = active.part_index;
        let written = catch_unwind(AssertUnwindSafe(|| {
            self.write_part(game, &mut active.asm, part_index)
        }))
        .unwrap_or_else(|_| Err(io::Error::other("finalize job panicked")));
        match written {
            Ok(path) => self.emit(RecorderEvent::SessionPartSaved {
                path,
                part_index,
                final_part,
            }),
            Err(e) => {
                let error = e.to_string();
                lock(kept).push(stage.clone());
                self.emit(RecorderEvent::SessionPartFailed {
                    part_index,
                    final_part,
                    error: error.clone(),
                    kept_stage: stage,
                });
                self.emit(RecorderEvent::Error(format!("session not saved: {error}")));
            }
        }
    }

    /// Concatenates the staged segments of `asm` into a new session part file
    /// and returns its path.
    fn write_part(
        &self,
        game: &str,
        asm: &mut SessionAssembler,
        part_index: u32,
    ) -> io::Result<PathBuf> {
        let stem = format!(
            "{game} {} session-part{part_index}",
            naming::utc_stamp(naming::unix_now()),
        );
        let dir = naming::game_dir(&self.cfg.sessions_dir, game, self.cfg.per_game_folders);
        fs::create_dir_all(&dir)?;
        let out = naming::unique_path(&dir, &stem, "mp4");
        Ok(asm.finalize(&out)?.path)
    }

    /// Deletes `run_dir`, except the stage directories in `kept`. When any are
    /// kept, the rest of the directory is deleted and the kept paths are reported.
    fn remove_run_dir(&self, run_dir: &Path, kept: &[PathBuf]) {
        if kept.is_empty() {
            if let Err(e) = fs::remove_dir_all(run_dir) {
                self.emit(RecorderEvent::Error(format!(
                    "could not delete {}: {e}",
                    run_dir.display()
                )));
            }
            return;
        }
        match fs::read_dir(run_dir) {
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if kept.contains(&path) {
                        continue;
                    }
                    let removed = if path.is_dir() {
                        fs::remove_dir_all(&path)
                    } else {
                        fs::remove_file(&path)
                    };
                    if let Err(e) = removed {
                        self.emit(RecorderEvent::Error(format!(
                            "could not delete {}: {e}",
                            path.display()
                        )));
                    }
                }
            }
            Err(e) => self.emit(RecorderEvent::Error(format!(
                "could not read {}: {e}",
                run_dir.display()
            ))),
        }
        let listed: Vec<String> = kept.iter().map(|p| p.display().to_string()).collect();
        self.emit(RecorderEvent::Error(format!(
            "staged session segments kept for recovery in {}",
            listed.join(", ")
        )));
    }
}

/// Counters and sets shared between the recorder and the finalizer.
#[derive(Default)]
struct Shared {
    /// Session part writes queued or running. Reported as `RecorderStatus::finalizing`.
    in_flight: AtomicU32,
    /// Jobs queued or running. [`Recorder::wait_idle`] waits for zero.
    pending: AtomicU32,
    /// Run directories whose `Cleanup` job is queued or running. The stale-run
    /// sweep skips them.
    busy: Mutex<HashSet<PathBuf>>,
}

/// Runs one job, then updates the counters. A panic in the job is reported as
/// an `Error` and does not stop the finalizer.
fn execute(ctx: &Ctx, shared: &Shared, job: Job) {
    let is_part = matches!(job, Job::Part { .. });
    let cleanup_dir = match &job {
        Job::Cleanup { run_dir, .. } => Some(run_dir.clone()),
        Job::Part { .. } => None,
    };
    let outcome = catch_unwind(AssertUnwindSafe(|| match job {
        Job::Part {
            game,
            active,
            final_part,
            kept,
        } => ctx.run_part(&game, active, final_part, &kept),
        Job::Cleanup {
            run_dir,
            kept,
            workers,
        } => {
            for handle in workers {
                let _ = handle.join();
            }
            let stages = lock(&kept).clone();
            ctx.remove_run_dir(&run_dir, &stages);
        }
    }));
    if outcome.is_err() {
        ctx.emit(RecorderEvent::Error("finalize job panicked".to_string()));
    }
    if let Some(dir) = cleanup_dir {
        lock(&shared.busy).remove(&dir);
    }
    if is_part {
        shared.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
    shared.pending.fetch_sub(1, Ordering::SeqCst);
}

/// The finalizer thread and its queue. Dropping `tx` lets the thread drain the
/// queue and exit.
struct Finalizer {
    tx: Option<Sender<Job>>,
    handle: Option<JoinHandle<()>>,
}

impl Finalizer {
    /// Starts the `clipcheck-finalize` thread. Returns `None` when the thread
    /// cannot be spawned. The recorder then runs jobs on the caller's thread.
    fn spawn(ctx: Arc<Ctx>, shared: Arc<Shared>) -> Option<Self> {
        let (tx, rx) = mpsc::channel::<Job>();
        let handle = thread::Builder::new()
            .name("clipcheck-finalize".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    execute(&ctx, &shared, job);
                }
            })
            .ok()?;
        Some(Self {
            tx: Some(tx),
            handle: Some(handle),
        })
    }
}

/// Records one attached window at a time. See the module docs.
pub struct Recorder {
    cfg: RecorderConfig,
    events: Sender<RecorderEvent>,
    run: Option<Run>,
    /// Clip worker threads that have not been joined yet.
    workers: Mutex<Vec<JoinHandle<()>>>,
    next_job: AtomicU64,
    /// Frame drop count from the last detached run.
    last_dropped: u64,
    /// Job state shared with the finalizer.
    ctx: Arc<Ctx>,
    shared: Arc<Shared>,
    /// The finalizer thread. `None` when it could not be spawned.
    finalizer: Option<Finalizer>,
}

impl Recorder {
    /// Creates an idle recorder. Events are sent on `events`. Starts the
    /// finalizer thread (D48). If it cannot start, session parts are written on
    /// the calling thread instead.
    pub fn new(cfg: RecorderConfig, events: Sender<RecorderEvent>) -> Self {
        let ctx = Arc::new(Ctx {
            cfg: cfg.clone(),
            events: events.clone(),
        });
        let shared = Arc::new(Shared::default());
        let finalizer = Finalizer::spawn(ctx.clone(), shared.clone());
        Self {
            cfg,
            events,
            run: None,
            workers: Mutex::new(Vec::new()),
            next_job: AtomicU64::new(0),
            last_dropped: 0,
            ctx,
            shared,
            finalizer,
        }
    }

    /// Starts recording the window `hwnd`. `game` is the label for file names
    /// and status. Starts a session at once when `session_auto_start` is set.
    pub fn attach(&mut self, hwnd: isize, game: &str) -> Result<(), RecorderError> {
        if self.run.is_some() {
            return Err(RecorderError::AlreadyAttached);
        }
        if self.cfg.audio_tracks.iter().any(|t| t.bitrate_kbps == 0) {
            return Err(RecorderError::InvalidRequest(
                "audio track bitrate must be non-zero".to_string(),
            ));
        }
        let run_dir = self.cfg.work_dir.join(naming::run_id());
        // The sweep is skipped while a finalizer cleanup is queued. The sweep
        // takes one directory to keep, so it cannot protect several. Nothing
        // can be added to the busy set while attached, so an empty set here
        // stays empty during the sweep. Stale folders are swept at a later attach.
        if lock(&self.shared.busy).is_empty() {
            self.sweep_stale_runs(&run_dir);
        }
        fs::create_dir_all(&run_dir).map_err(io_err)?;
        let started = Instant::now();

        // Audio first, so the pipeline can take its channels. Dropping the
        // feeder on an error below stops it.
        let (audio, lanes, startup_warnings) = if self.cfg.audio_tracks.is_empty() {
            (None, Vec::new(), Vec::new())
        } else {
            let pid = window_pid(hwnd);
            let (feeder, receivers) =
                AudioFeeder::start(&self.cfg.audio_tracks, pid).map_err(io_err)?;
            let lanes: AudioLanes = self
                .cfg
                .audio_tracks
                .iter()
                .zip(receivers)
                .map(|(track, rx)| {
                    let spec = AudioTrackSpec {
                        title: track.title.clone(),
                        bitrate_kbps: track.bitrate_kbps,
                    };
                    (spec, rx)
                })
                .collect();
            let warnings = feeder.warnings();
            (Some(feeder), lanes, warnings)
        };

        let canvas = choose_canvas(self.cfg.canvas, self.cfg.canvas_height, window_size(hwnd));
        let capture = match start_capture(&self.cfg, hwnd, &run_dir, lanes, canvas) {
            Ok(capture) => capture,
            Err(e) => {
                let _ = fs::remove_dir_all(&run_dir);
                return Err(e);
            }
        };

        let ring_secs = if self.cfg.buffer_enabled {
            self.cfg.buffer_max_secs
        } else {
            DISABLED_RING_SECS
        };
        let index = Arc::new(Mutex::new(RingIndex::new()));
        let session = Arc::new(Mutex::new(None));
        let tracker = SegmentTracker::new(run_dir.clone(), ring_secs, index.clone());
        let sink = segment_sink(session.clone(), self.events.clone());
        let tracker = TrackerThread::spawn(tracker, TRACKER_POLL, sink);

        for warning in startup_warnings {
            self.emit(RecorderEvent::Warning(warning));
        }
        let audio_watch = match audio.as_ref() {
            Some(feeder) => {
                match AudioWatch::spawn(feeder.warnings_handle(), self.events.clone()) {
                    Ok(watch) => Some(watch),
                    Err(e) => {
                        self.emit(RecorderEvent::Error(format!(
                            "audio warning watch not started: {e}"
                        )));
                        None
                    }
                }
            }
            None => None,
        };
        self.last_dropped = 0;
        self.run = Some(Run {
            label: game.to_string(),
            game: naming::sanitize(game),
            run_dir,
            started,
            capture: Some(capture),
            audio,
            audio_watch,
            tracker: Some(tracker),
            index,
            session,
            session_seq: 0,
            kept_stages: Arc::new(Mutex::new(Vec::new())),
        });
        if self.cfg.session_auto_start {
            if let Err(e) = self.session_start() {
                self.emit(RecorderEvent::Error(format!(
                    "could not start session: {e}"
                )));
            }
        }
        Ok(())
    }

    /// Stops the capture, queues the final session part, and queues deletion of
    /// the run directory. Returns to idle. Does not wait for the queued work;
    /// see [`Recorder::wait_idle`]. The run directory is kept when a part could
    /// not be written. Calling it while idle does nothing.
    ///
    /// The capture, audio and tracker still stop synchronously, so their
    /// warnings are emitted before this returns. Only the part write and the
    /// directory cleanup are asynchronous (D48).
    pub fn detach(&mut self) {
        let Some(mut run) = self.run.take() else {
            return;
        };
        // Audio stops first. Its channels then disconnect, and the pipeline
        // writes what it already holds before the video stops.
        if let Some(mut watch) = run.audio_watch.take() {
            watch.stop();
        }
        if let Some(mut feeder) = run.audio.take() {
            feeder.stop();
            for warning in feeder.warnings() {
                self.emit(RecorderEvent::Warning(warning));
            }
        }
        if let Some(capture) = run.capture.take() {
            match stop_capture(capture) {
                Ok(dropped) => self.last_dropped = dropped,
                Err(e) => self.emit(RecorderEvent::Error(format!("capture stop failed: {e}"))),
            }
        }
        // The final tracker pass stages the last segment into the session, so
        // the session is taken only after it returns.
        if let Some(tracker) = run.tracker.take() {
            if let Err(e) = tracker.stop() {
                self.emit(RecorderEvent::Error(format!("segment tracker failed: {e}")));
            }
        }
        let active = lock(&run.session).take();
        if let Some(active) = active {
            self.submit(Job::Part {
                game: run.game.clone(),
                active,
                final_part: true,
                kept: run.kept_stages.clone(),
            });
        }
        // Clip workers are joined by the cleanup job, after their work. Mark
        // the run directory busy first, so a sweep cannot delete it meanwhile.
        let workers: Vec<JoinHandle<()>> = std::mem::take(&mut *lock(&self.workers));
        lock(&self.shared.busy).insert(run.run_dir.clone());
        self.submit(Job::Cleanup {
            run_dir: run.run_dir.clone(),
            kept: run.kept_stages.clone(),
            workers,
        });
    }

    /// Removes stale run directories left by crashed sessions under `work_dir`,
    /// keeping `current`. Best effort: problems are reported as events and never
    /// fail the caller. See [`crate::sweep`].
    fn sweep_stale_runs(&self, current: &Path) {
        let report: SweepReport = sweep::sweep_stale_runs(
            &self.cfg.work_dir,
            current,
            SystemTime::now(),
            sweep::STALE_AFTER,
        );
        for path in &report.kept_for_salvage {
            self.emit(RecorderEvent::Warning(format!(
                "stale run folder kept because it holds unsaved session parts: {}",
                path.display()
            )));
        }
        for error in report.errors {
            self.emit(RecorderEvent::Error(format!("stale run sweep: {error}")));
        }
    }

    /// Starts saving a session. Segments completed from now on are staged into
    /// part 1.
    pub fn session_start(&mut self) -> Result<(), RecorderError> {
        let run = self.run.as_mut().ok_or(RecorderError::NotAttached)?;
        let mut slot = lock(&run.session);
        if slot.is_some() {
            return Err(RecorderError::SessionActive);
        }
        run.session_seq += 1;
        *slot = Some(new_session(&run.run_dir, run.session_seq, 1)?);
        Ok(())
    }

    /// Ends the current session part, queues its write, and starts part N+1
    /// with no gap. Returns at once, without waiting for the write (D48).
    ///
    /// The boundary is the moment of the call. Segments already staged go into
    /// part N, and the next segment to complete goes into part N+1. Segments are
    /// cut on keyframes, so the two parts join without a gap. The write outcome
    /// arrives as [`RecorderEvent::SessionPartSaved`] or
    /// [`RecorderEvent::SessionPartFailed`]. Parts are written in order. Fails
    /// with [`RecorderError::NoSegments`] when part N holds no segments yet.
    pub fn session_split(&mut self) -> Result<(), RecorderError> {
        let run = self.run.as_mut().ok_or(RecorderError::NotAttached)?;
        let mut slot = lock(&run.session);
        let current = slot.as_mut().ok_or(RecorderError::NoSession)?;
        if current.asm.segment_count() == 0 {
            return Err(RecorderError::NoSegments);
        }
        let next_part = current.part_index + 1;
        let next = new_session(&run.run_dir, run.session_seq, next_part)?;
        // `current` is the live part, so swapping it out returns the old part.
        let old = std::mem::replace(current, next);
        let game = run.game.clone();
        let kept = run.kept_stages.clone();
        drop(slot);
        self.submit(Job::Part {
            game,
            active: old,
            final_part: false,
            kept,
        });
        Ok(())
    }

    /// Ends the session and queues its final part for writing. Returns at once
    /// (D48). A session with no staged segments queues nothing. Calling it with
    /// no session active does nothing. The replay buffer keeps running. Write
    /// failures are reported as events, not returned.
    pub fn session_stop(&mut self) -> Result<(), RecorderError> {
        let run = self.run.as_ref().ok_or(RecorderError::NotAttached)?;
        let old = lock(&run.session).take();
        if let Some(active) = old {
            self.submit(Job::Part {
                game: run.game.clone(),
                active,
                final_part: true,
                kept: run.kept_stages.clone(),
            });
        }
        Ok(())
    }

    /// Queues a clip of the last `seconds` seconds of the buffer.
    ///
    /// The plan is made now, so buffer errors are returned at once. Extraction
    /// runs on a worker thread, and the outcome arrives as
    /// [`RecorderEvent::ClipSaved`] or [`RecorderEvent::ClipFailed`]. Works with
    /// or without a session.
    pub fn clip(&self, seconds: u32) -> Result<JobId, RecorderError> {
        let run = self.run.as_ref().ok_or(RecorderError::NotAttached)?;
        if !self.cfg.buffer_enabled {
            return Err(RecorderError::BufferDisabled);
        }
        if seconds == 0 {
            return Err(RecorderError::InvalidRequest(
                "clip length must be at least 1 second".to_string(),
            ));
        }
        let now = run.now_100ns();
        let (plan, lease) = {
            let mut idx = lock(&run.index);
            // Anchored to the end of the recorded footage, not the wall clock: the
            // index timeline trails the wall clock, and a wall anchor shrinks the
            // clip to one segment once the gap exceeds the requested length.
            let (plan, anchor) = idx
                .plan_latest_clip(now, u64::from(seconds), 0)
                .map_err(plan_err)?;
            let start = plan.segments.first().map_or(anchor, |s| s.start_100ns);
            let lease = idx.lease(Window {
                start_100ns: start,
                end_100ns: anchor,
            });
            (plan, lease)
        };
        let guard = LeaseGuard {
            index: run.index.clone(),
            lease: Some(lease),
        };

        let job = JobId(self.next_job.fetch_add(1, Ordering::Relaxed) + 1);
        let stem = format!(
            "{} {} clip-{seconds}s",
            run.game,
            naming::utc_stamp(naming::unix_now())
        );
        let dir = naming::game_dir(&self.cfg.clips_dir, &run.game, self.cfg.per_game_folders);
        let out = naming::unique_path(&dir, &stem, "mp4");
        let events = self.events.clone();
        let (actual, short) = (plan.effective_secs, plan.short);
        self.spawn_worker("clipcheck-clip", move || {
            let result = fs::create_dir_all(&dir).and_then(|()| extract_clip(&plan, &out));
            // The lease covers extraction only. Release it before reporting.
            drop(guard);
            let event = match result {
                Ok(_) => RecorderEvent::ClipSaved {
                    job,
                    path: out,
                    seconds_requested: seconds,
                    seconds_actual: actual,
                    short,
                },
                Err(e) => RecorderEvent::ClipFailed {
                    job,
                    error: e.to_string(),
                },
            };
            let _ = events.send(event);
        })
        .map_err(io_err)?;
        Ok(job)
    }

    /// Per-track audio levels in dBFS, one entry per track. Empty when no audio
    /// feeder is running.
    ///
    /// `peak_db` is the largest peak since the previous call. `rms_db` is the RMS
    /// of the latest mixed block. Each call resets the peaks. The daemon polls
    /// this about 10 times a second.
    pub fn audio_levels(&self) -> Vec<LevelSnapshot> {
        match self.run.as_ref().and_then(|run| run.audio.as_ref()) {
            Some(feeder) => feeder.levels().take(),
            None => Vec::new(),
        }
    }

    /// Reports the current state. Does not block on recording.
    pub fn status(&self) -> RecorderStatus {
        let finalizing = self.shared.in_flight.load(Ordering::SeqCst);
        let Some(run) = self.run.as_ref() else {
            return RecorderStatus {
                attached: None,
                session_active: false,
                part_index: 0,
                buffer_secs: 0.0,
                frames_dropped: self.last_dropped,
                finalizing,
            };
        };
        let (session_active, part_index) = match lock(&run.session).as_ref() {
            Some(active) => (true, active.part_index),
            None => (false, 0),
        };
        let buffer_secs = if self.cfg.buffer_enabled {
            let idx = lock(&run.index);
            // Reads the two end segments in O(1) rather than walking the ring.
            // An empty index yields (None, None); no separate length check is needed.
            let first = idx.first();
            let last = idx.last();
            match (first, last) {
                (Some(first), Some(last)) => {
                    let span = last.end_100ns.saturating_sub(first.start_100ns).max(0);
                    span as f64 / TICKS_PER_SEC as f64
                }
                _ => 0.0,
            }
        } else {
            0.0
        };
        RecorderStatus {
            attached: Some(run.label.clone()),
            session_active,
            part_index,
            buffer_secs,
            frames_dropped: self.last_dropped,
            finalizing,
        }
    }

    /// Waits until the finalizer has no queued or running jobs. Returns true
    /// when idle, or false when `timeout` passes first. Session parts and run
    /// cleanup are then done. Polls every 10 ms.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.shared.pending.load(Ordering::SeqCst) == 0 {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            thread::sleep(WAIT_IDLE_POLL.min(deadline - now));
        }
    }

    /// Hands `job` to the finalizer thread, or runs it on this thread when the
    /// thread is unavailable. Jobs are counted as pending until they finish.
    fn submit(&self, job: Job) {
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        if matches!(job, Job::Part { .. }) {
            self.shared.in_flight.fetch_add(1, Ordering::SeqCst);
        }
        if let Some(tx) = self.finalizer.as_ref().and_then(|f| f.tx.as_ref()) {
            match tx.send(job) {
                Ok(()) => return,
                Err(mpsc::SendError(job)) => return execute(&self.ctx, &self.shared, job),
            }
        }
        execute(&self.ctx, &self.shared, job);
    }

    /// Runs `f` on a named worker thread and tracks its handle so that
    /// [`Recorder::detach`] can wait for it.
    fn spawn_worker(&self, name: &str, f: impl FnOnce() + Send + 'static) -> io::Result<()> {
        let handle = thread::Builder::new().name(name.to_string()).spawn(f)?;
        let mut workers = lock(&self.workers);
        workers.retain(|h| !h.is_finished());
        workers.push(handle);
        Ok(())
    }

    /// Waits for every tracked worker to finish.
    fn join_workers(&self) {
        let handles: Vec<JoinHandle<()>> = std::mem::take(&mut *lock(&self.workers));
        for handle in handles {
            let _ = handle.join();
        }
    }

    /// Sends an event. Ignores a disconnected receiver.
    fn emit(&self, event: RecorderEvent) {
        let _ = self.events.send(event);
    }
}

impl Drop for Recorder {
    /// Detaches, then waits without a deadline for the finalizer to write every
    /// queued part. A shutdown never abandons a part (D48).
    fn drop(&mut self) {
        self.detach();
        if let Some(mut finalizer) = self.finalizer.take() {
            // Dropping the sender lets the thread drain the queue and exit.
            let _ = finalizer.tx.take();
            if let Some(handle) = finalizer.handle.take() {
                let _ = handle.join();
            }
        }
        self.join_workers();
    }
}

#[cfg(test)]
mod warning_tests {
    use super::*;

    #[test]
    fn fresh_warnings_reports_each_message_once() {
        let mut seen = HashSet::new();
        let first = fresh_warnings(&mut seen, vec!["a".into(), "b".into()]);
        assert_eq!(first, vec!["a".to_string(), "b".to_string()]);
        let second = fresh_warnings(&mut seen, vec!["b".into(), "c".into()]);
        assert_eq!(second, vec!["c".to_string()]);
        assert!(fresh_warnings(&mut seen, Vec::new()).is_empty());
    }
}

#[cfg(test)]
mod canvas_choice_tests {
    use super::choose_canvas;

    const FALLBACK: (u32, u32) = (1280, 720);

    #[test]
    fn unknown_window_size_uses_fallback() {
        assert_eq!(choose_canvas(FALLBACK, 1440, None), (1280, 720));
    }

    #[test]
    fn ultrawide_at_fixed_height_follows_source() {
        assert_eq!(
            choose_canvas(FALLBACK, 1440, Some((3440, 1440))),
            (3440, 1440)
        );
    }

    #[test]
    fn native_height_uses_source_size() {
        assert_eq!(choose_canvas(FALLBACK, 0, Some((1920, 1080))), (1920, 1080));
    }

    #[test]
    fn implausibly_small_window_uses_fallback() {
        assert_eq!(choose_canvas(FALLBACK, 1440, Some((50, 50))), (1280, 720));
    }

    #[test]
    fn very_wide_window_is_clamped_to_max_width() {
        assert_eq!(
            choose_canvas(FALLBACK, 1440, Some((5120, 1440))),
            (3840, 1080)
        );
    }
}
