//! The library worker thread: root sync, chunked scans, thumbnails and library commands.
//!
//! The worker owns one library connection. Work is cut into steps so that IPC commands are
//! answered between steps. A walk runs on its own thread. A probe chunk holds at most
//! [`CHUNK`] files. Thumbnails come in passes of [`THUMBS_PER_PASS`]. No step starts while a
//! recording session is active, and a probe chunk stops early when one starts. A stopped
//! chunk restarts its root from a fresh walk, which keeps the rows consistent.

mod helpers;
mod ipc;

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use clipcheck_engine::scan::{
    apply_scan, generate_missing_thumbnails, plan_scan_checked, walk_root, FoundFile, RootTarget,
    ScanOptions, ScanPlan, ScanProgress, ScanReport, Walk,
};
use clipcheck_ipc::proto::{Command, Event, Reply};
use clipcheck_library::{Library, PathRow, RootInfo};

use helpers::{chunk_plan, path_under, publish_scan};

use super::sync;
use super::{Shared, Throttle};
use crate::logging;

/// Files probed per step. Keeps one step, and so one IPC wait, to a few seconds.
const CHUNK: usize = 12;
/// Posters made per thumbnail pass.
const THUMBS_PER_PASS: usize = 20;
/// Delay before the first scan of every root after startup.
const FIRST_SCAN_DELAY: Duration = Duration::from_secs(5);
/// Interval between scans of every root while the daemon runs.
const RESCAN_EVERY: Duration = Duration::from_secs(15 * 60);
/// Pause between two steps, so scans take a fraction of the disk and CPU.
const STEP_GAP: Duration = Duration::from_millis(20);
/// Wait between idle ticks.
const TICK: Duration = Duration::from_millis(200);
/// Minimum time between two progress events of one kind.
const EVENT_EVERY_MS: u64 = 250;
/// Poll interval while a walk waits for a recording to end.
const PAUSE_POLL: Duration = Duration::from_millis(200);
/// Thumbnail folder inside the data directory.
const THUMBS_DIR: &str = "thumbs";

/// Messages the worker receives.
pub(crate) enum Cmd {
    /// A library command from an IPC thread, with the channel for its reply.
    Ipc(Command, Sender<Reply>),
    /// The storage settings changed.
    StorageChanged,
    /// A walk thread finished.
    WalkDone {
        /// Root that was walked.
        root_id: i64,
        /// The walk, or the error that stopped it.
        result: io::Result<Walk>,
    },
    /// Stop the worker.
    Stop,
}

/// One file to probe.
enum Item {
    /// A file with no row yet.
    New(FoundFile),
    /// A row whose file changed. Holds the row id.
    Changed(String, FoundFile),
}

/// Counts for one scan, reported in the scan events.
#[derive(Debug, Clone, Copy, Default)]
struct Totals {
    added: u64,
    updated: u64,
    missing: u64,
    failed: u64,
    thumbs: u64,
}

/// Where a root's scan stands.
enum Phase {
    /// The walk thread is running.
    Walking,
    /// Files are probed in chunks; `missing` rows are marked at the end.
    Probing {
        items: VecDeque<Item>,
        total: usize,
        done: usize,
        missing: Vec<String>,
    },
    /// Posters are made in passes until none are left.
    Thumbs,
}

/// The root being scanned.
struct Active {
    root_id: i64,
    path: PathBuf,
    phase: Phase,
    totals: Totals,
    /// Stops the walk when set.
    cancel: Arc<AtomicBool>,
    /// Rows of the root read before the walk started. A recording registered during the walk is
    /// not in this list, so the walk can never mark it missing.
    snapshot: Vec<PathRow>,
}

/// What a step decided about the active scan.
enum Outcome {
    /// Keep the scan for the next step.
    Keep,
    /// The scan is complete.
    Done,
    /// The scan must start again from a fresh walk.
    Restart,
}

/// State owned by the worker thread.
struct Worker {
    lib: Library,
    shared: Arc<Shared>,
    tx: Sender<Cmd>,
    /// Roots waiting for a scan, in order.
    pending: VecDeque<i64>,
    active: Option<Active>,
    /// When every root is scanned next.
    next_rescan: Instant,
    scan_throttle: Throttle,
    changed_throttle: Throttle,
    /// Rows changed since the last `library_changed` event.
    changed_dirty: bool,
}

/// Thread body: syncs the roots, then handles commands and runs scan steps until stopped.
pub(crate) fn run(lib: Library, rx: Receiver<Cmd>, tx: Sender<Cmd>, shared: Arc<Shared>) {
    let mut worker = Worker {
        lib,
        shared,
        tx,
        pending: VecDeque::new(),
        active: None,
        next_rescan: Instant::now() + FIRST_SCAN_DELAY,
        scan_throttle: Throttle::new(EVENT_EVERY_MS),
        changed_throttle: Throttle::new(EVENT_EVERY_MS),
        changed_dirty: false,
    };
    worker.sync_roots();
    loop {
        let wait = if worker.can_step() && worker.has_work() {
            STEP_GAP
        } else {
            TICK
        };
        match rx.recv_timeout(wait) {
            Ok(cmd) => {
                if worker.handle(cmd) {
                    break;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let mut stop = false;
        while let Ok(cmd) = rx.try_recv() {
            if worker.handle(cmd) {
                stop = true;
                break;
            }
        }
        if stop {
            break;
        }
        worker.tick();
    }
    if let Some(active) = worker.active.take() {
        active.cancel.store(true, Ordering::SeqCst);
    }
    logging::info("library worker stopped");
}

impl Worker {
    /// True when the worker may start scan or thumbnail work now.
    fn can_step(&self) -> bool {
        !self.shared.recording() && !self.shared.stopping() && self.shared.is_ready()
    }

    fn has_work(&self) -> bool {
        self.active.is_some() || !self.pending.is_empty()
    }

    /// Handles one message. Returns true when the worker must stop.
    fn handle(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Stop => return true,
            Cmd::Ipc(command, reply) => {
                let answer = self.ipc(command);
                let _ = reply.send(answer);
            }
            Cmd::StorageChanged => self.sync_roots(),
            Cmd::WalkDone { root_id, result } => self.walk_done(root_id, result),
        }
        false
    }

    /// Runs the periodic rescan and one scan step, then flushes throttled events.
    fn tick(&mut self) {
        let now = Instant::now();
        if now >= self.next_rescan && self.can_step() {
            self.enqueue_all();
            self.next_rescan = now + RESCAN_EVERY;
        }
        if self.can_step() {
            self.step();
        }
        self.flush_changed(false);
    }

    /// Does one unit of scan work: starts the next pending walk, or advances the active scan.
    fn step(&mut self) {
        let Some(mut active) = self.active.take() else {
            if let Some(root_id) = self.pending.pop_front() {
                self.start_walk(root_id);
            }
            return;
        };
        match self.advance(&mut active) {
            Outcome::Keep => self.active = Some(active),
            Outcome::Done => self.finish(&active),
            Outcome::Restart => self.pending.push_front(active.root_id),
        }
    }

    /// Advances the active scan by one step.
    fn advance(&mut self, active: &mut Active) -> Outcome {
        if !self.shared.is_ready() {
            return Outcome::Keep;
        }
        let phase = std::mem::replace(&mut active.phase, Phase::Thumbs);
        match phase {
            Phase::Walking => {
                active.phase = Phase::Walking;
                Outcome::Keep
            }
            Phase::Probing {
                mut items,
                total,
                done,
                missing,
            } => {
                if items.is_empty() {
                    // Every file is probed: mark the missing rows and stamp the scan.
                    let plan = ScanPlan {
                        missing,
                        ..ScanPlan::default()
                    };
                    self.apply(active, &plan);
                    active.phase = Phase::Thumbs;
                    return Outcome::Keep;
                }
                let count = items.len().min(CHUNK);
                let chunk: Vec<Item> = items.drain(..count).collect();
                active.phase = Phase::Probing {
                    items,
                    total,
                    done: done + count,
                    missing,
                };
                self.probe_chunk(active, chunk, done, total)
            }
            Phase::Thumbs => {
                let thumbs_dir = self.shared.data_dir.join(THUMBS_DIR);
                let made =
                    match generate_missing_thumbnails(&self.lib, &thumbs_dir, THUMBS_PER_PASS) {
                        Ok(made) => made,
                        Err(e) => {
                            self.warn(format!("thumbnails not made: {e}"));
                            0
                        }
                    };
                if made == 0 {
                    return Outcome::Done;
                }
                active.totals.thumbs += made as u64;
                active.phase = Phase::Thumbs;
                if self.scan_throttle.ready() {
                    publish_scan(
                        &self.shared.bus,
                        active.root_id,
                        &active.path.display().to_string(),
                        "thumbnails",
                        active.totals.thumbs,
                        0,
                        &active.totals,
                    );
                }
                Outcome::Keep
            }
        }
    }

    /// Probes one chunk of files and writes the rows. `base` is the number of files done before.
    fn probe_chunk(
        &mut self,
        active: &mut Active,
        chunk: Vec<Item>,
        base: usize,
        total: usize,
    ) -> Outcome {
        let plan = chunk_plan(chunk);
        let cancel = AtomicBool::new(false);
        let totals = active.totals;
        let root_id = active.root_id;
        let path_text = active.path.display().to_string();
        let result = {
            let shared = &self.shared;
            let throttle = &mut self.scan_throttle;
            let mut progress = |p: ScanProgress| {
                if shared.recording() || shared.stopping() {
                    cancel.store(true, Ordering::SeqCst);
                }
                if let ScanProgress::Probing { done, .. } = p {
                    if throttle.ready() {
                        publish_scan(
                            &shared.bus,
                            root_id,
                            &path_text,
                            "probing",
                            (base + done) as u64,
                            total as u64,
                            &totals,
                        );
                    }
                }
            };
            apply_scan(
                &self.lib,
                RootTarget {
                    root_id,
                    root: &active.path,
                },
                &plan,
                &ScanOptions::default(),
                &mut progress,
                &cancel,
            )
        };
        match result {
            Ok(report) if report.cancelled => Outcome::Restart,
            Ok(report) => {
                self.absorb(active, &report);
                Outcome::Keep
            }
            Err(e) => {
                self.warn(format!("probe of {path_text} failed: {e}"));
                active.totals.failed += 1;
                Outcome::Keep
            }
        }
    }

    /// Writes the missing marks of a scan (a plan without new or changed files).
    fn apply(&mut self, active: &mut Active, plan: &ScanPlan) {
        let cancel = AtomicBool::new(false);
        let result = apply_scan(
            &self.lib,
            RootTarget {
                root_id: active.root_id,
                root: &active.path,
            },
            plan,
            &ScanOptions::default(),
            &mut |_: ScanProgress| {},
            &cancel,
        );
        match result {
            Ok(report) => self.absorb(active, &report),
            Err(e) => {
                self.warn(format!(
                    "marking missing files in {} failed: {e}",
                    active.path.display()
                ));
                active.totals.failed += 1;
            }
        }
    }

    /// Adds one report to the scan totals and notes changed rows.
    fn absorb(&mut self, active: &mut Active, report: &ScanReport) {
        active.totals.added += report.added as u64;
        active.totals.updated += report.updated as u64;
        active.totals.missing += report.marked_missing as u64;
        active.totals.failed += report.failed.len() as u64;
        for (path, error) in &report.failed {
            logging::warn(&format!("probe failed for {path}: {error}"));
        }
        if report.added + report.updated + report.marked_missing > 0 {
            self.changed_dirty = true;
        }
    }

    /// Reports a finished scan.
    fn finish(&mut self, active: &Active) {
        self.flush_changed(true);
        let path = active.path.display().to_string();
        publish_scan(
            &self.shared.bus,
            active.root_id,
            &path,
            "done",
            0,
            0,
            &active.totals,
        );
        logging::info(&format!(
            "scan of {path} done: {} added, {} updated, {} missing, {} failed, {} thumbnails",
            active.totals.added,
            active.totals.updated,
            active.totals.missing,
            active.totals.failed,
            active.totals.thumbs
        ));
    }

    /// Starts the walk of one root on its own thread.
    fn start_walk(&mut self, root_id: i64) {
        let Some(row) = self.find_root(root_id) else {
            return;
        };
        let path = PathBuf::from(&row.path);
        let cancel = Arc::new(AtomicBool::new(false));
        let walk_cancel = Arc::clone(&cancel);
        let shared = Arc::clone(&self.shared);
        let tx = self.tx.clone();
        let path_text = row.path.clone();
        publish_scan(
            &self.shared.bus,
            root_id,
            &path_text,
            "walking",
            0,
            0,
            &Totals::default(),
        );
        let rows = match self.lib.list_paths_for_root(root_id) {
            Ok(rows) => rows,
            Err(e) => {
                self.warn(format!("scan of {} stopped: {e}", row.path));
                return;
            }
        };
        let walk_path = path.clone();
        let spawned = thread::Builder::new()
            .name("library-walk".to_owned())
            .spawn(move || {
                let mut throttle = Throttle::new(EVENT_EVERY_MS);
                let result = walk_root(
                    &walk_path,
                    &ScanOptions::default(),
                    &walk_cancel,
                    &mut |found| {
                        while shared.recording()
                            && !shared.stopping()
                            && !walk_cancel.load(Ordering::SeqCst)
                        {
                            thread::sleep(PAUSE_POLL);
                        }
                        if throttle.ready() {
                            publish_scan(
                                &shared.bus,
                                root_id,
                                &path_text,
                                "walking",
                                found as u64,
                                0,
                                &Totals::default(),
                            );
                        }
                    },
                );
                let _ = tx.send(Cmd::WalkDone { root_id, result });
            });
        match spawned {
            Ok(_) => {
                self.active = Some(Active {
                    root_id,
                    path,
                    phase: Phase::Walking,
                    totals: Totals::default(),
                    cancel,
                    snapshot: rows,
                });
            }
            Err(e) => self.warn(format!("cannot start the walk of {}: {e}", row.path)),
        }
    }

    /// Diffs a finished walk against the root's rows and queues the probes.
    fn walk_done(&mut self, root_id: i64, result: io::Result<Walk>) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        if active.root_id != root_id || !matches!(active.phase, Phase::Walking) {
            self.active = Some(active);
            return;
        }
        let path_text = active.path.display().to_string();
        let walk = match result {
            Ok(walk) => walk,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => return,
            Err(_) => {
                // The folder cannot be read: offline. Rows are kept, nothing is marked missing.
                let _ = self.lib.set_root_availability(root_id, false);
                publish_scan(
                    &self.shared.bus,
                    root_id,
                    &path_text,
                    "offline",
                    0,
                    0,
                    &active.totals,
                );
                return;
            }
        };
        let _ = self.lib.set_root_availability(root_id, true);
        // Rows are the snapshot taken when the walk started. A row checked against the disk
        // again here is still missing only if its file is gone.
        let rows = std::mem::take(&mut active.snapshot);
        let root_dir = active.path.clone();
        // Rows under a folder that could not be read say nothing about the disk.
        let existing: Vec<_> = rows
            .into_iter()
            .filter(|row| !walk.unreadable.iter().any(|u| path_under(&row.1, u)))
            .collect();
        let plan = plan_scan_checked(&walk.files, &existing, &|rel| root_dir.join(rel).exists());
        let mut items: VecDeque<Item> =
            VecDeque::with_capacity(plan.new.len() + plan.changed.len());
        items.extend(plan.new.into_iter().map(Item::New));
        items.extend(
            plan.changed
                .into_iter()
                .map(|(id, file)| Item::Changed(id, file)),
        );
        let total = items.len();
        publish_scan(
            &self.shared.bus,
            root_id,
            &path_text,
            "probing",
            0,
            total as u64,
            &active.totals,
        );
        active.phase = Phase::Probing {
            items,
            total,
            done: 0,
            missing: plan.missing,
        };
        self.active = Some(active);
    }

    /// Makes the library's roots match the configuration and queues the new ones.
    fn sync_roots(&mut self) {
        let desired = sync::desired_roots(&self.shared.storage());
        let outcome = match sync::sync_roots(&self.lib, &desired) {
            Ok(outcome) => outcome,
            Err(e) => {
                self.warn(format!("library folders not updated: {e}"));
                return;
            }
        };
        for message in outcome.warnings {
            self.warn(message);
        }
        self.shared.set_primary_read_only(outcome.primary_read_only);
        self.pending.retain(|id| outcome.ids.contains(id));
        let active_removed = self
            .active
            .as_ref()
            .is_some_and(|active| !outcome.ids.contains(&active.root_id));
        if active_removed {
            if let Some(active) = self.active.take() {
                active.cancel.store(true, Ordering::SeqCst);
            }
        }
        // Every kept root is rescanned after a change, so a new or edited folder is indexed.
        for id in outcome.ids {
            self.enqueue(id);
        }
        self.changed_dirty = true;
        self.flush_changed(true);
    }

    /// Queues a root for a scan unless it is already queued or being scanned.
    fn enqueue(&mut self, root_id: i64) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.root_id == root_id)
            || self.pending.contains(&root_id)
        {
            return;
        }
        self.pending.push_back(root_id);
    }

    /// Queues every root for a scan.
    fn enqueue_all(&mut self) {
        match self.lib.list_roots_with_stats() {
            Ok(rows) => {
                for row in rows {
                    self.enqueue(row.id);
                }
            }
            Err(e) => self.warn(format!("library roots not listed: {e}")),
        }
    }

    /// Sends the `library_changed` event, at most once per interval unless forced.
    fn flush_changed(&mut self, force: bool) {
        if !self.changed_dirty {
            return;
        }
        if force || self.changed_throttle.ready() {
            self.changed_dirty = false;
            self.shared.bus.publish(Event::LibraryChanged {});
        }
    }

    /// Logs a non-fatal problem and sends it as a `warning` event.
    fn warn(&self, message: String) {
        logging::warn(&message);
        self.shared.bus.publish(Event::Warning {
            code: "library".to_owned(),
            message,
        });
    }

    fn find_root(&self, root_id: i64) -> Option<RootInfo> {
        self.lib
            .list_roots_with_stats()
            .ok()?
            .into_iter()
            .find(|row| row.id == root_id)
    }

    fn is_scanning(&self, root_id: i64) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.root_id == root_id)
            || self.pending.contains(&root_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_under_matches_whole_components() {
        assert!(path_under(r"Clips\Game\a.mp4", r"Clips\Game"));
        assert!(path_under(r"Clips\Game", r"Clips\Game"));
        assert!(!path_under(r"Clips\GameTwo\a.mp4", r"Clips\Game"));
    }

    #[test]
    fn chunk_plan_splits_new_and_changed() {
        let file = FoundFile {
            rel_path: "a.mp4".to_owned(),
            size: 1,
            mtime_ms: 1,
        };
        let plan = chunk_plan(vec![
            Item::New(file.clone()),
            Item::Changed("id".to_owned(), file),
        ]);
        assert_eq!(plan.new.len(), 1);
        assert_eq!(plan.changed.len(), 1);
        assert!(plan.missing.is_empty());
    }

    #[test]
    fn throttle_allows_one_event_per_interval() {
        let mut throttle = Throttle::new(60_000);
        assert!(throttle.ready());
        assert!(!throttle.ready());
    }
}
