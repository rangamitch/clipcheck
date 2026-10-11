//! Multi-location library: root sync, background scans, thumbnails and imports (DESIGN.md D24).
//!
//! [`LibraryService`] is owned by the recording [`crate::service::Service`]. It runs its work
//! off the service thread, so recording and detection never wait for it:
//!
//! - The `library` worker thread (see `worker`) opens its own connection to `library.db`.
//!   It reconciles storage roots with the configuration, scans roots in small steps, makes
//!   missing thumbnails and answers the library IPC commands. It does no work while a
//!   recording session is active.
//! - Each import job runs on its own `library-import` thread (see `import`) with its own
//!   connection. Only one job runs at a time.
//!
//! Scan walks run on short `library-walk` threads. SQLite serialises the writes of the
//! separate connections (busy timeout 5 s), so the service's own library handle is not held
//! during a scan.

mod import;
pub(crate) mod sync;
mod worker;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use clipcheck_core::config::schema::Storage;
use clipcheck_ipc::proto::{Command, ErrorCode, Reply};
use clipcheck_library::Library;
pub(crate) use clipcheck_util::sync::lock_unpoisoned as lock;
use ulid::Ulid;

use crate::app::LIBRARY_FILE;
use crate::events::EventBus;

/// How long an IPC request waits for the library worker before answering `busy`.
const IPC_TIMEOUT: Duration = Duration::from_secs(5);

/// Everything the library service needs at startup.
pub struct LibraryOptions {
    /// Daemon data directory. Holds `library.db` and the `thumbs` folder.
    pub data_dir: PathBuf,
    /// Storage settings in effect: the primary root and the extra library folders.
    pub storage: Storage,
    /// Bus that carries library events.
    pub bus: Arc<EventBus>,
    /// Set by the recording service while a session is being written. Scans pause then.
    pub recording: Arc<AtomicBool>,
}

/// The import job that is running, if any.
struct ImportSlot {
    job_id: String,
    cancel: Arc<AtomicBool>,
}

/// State shared by the service thread, the worker, scan walks, import jobs and IPC threads.
pub(crate) struct Shared {
    /// Daemon data directory.
    pub data_dir: PathBuf,
    /// Bus for library events.
    pub bus: Arc<EventBus>,
    /// True while a recording session is being written.
    recording: Arc<AtomicBool>,
    /// Set when the service is stopping. Long steps check it.
    stopping: AtomicBool,
    /// Storage settings in effect.
    storage: Mutex<Storage>,
    /// Set once the encoder probe has finished. Scans and imports wait for it.
    ready: AtomicBool,
    /// The running import job. `None` when no import runs.
    import: Mutex<Option<ImportSlot>>,
    /// True when the primary root row is read-only (set by the worker's root sync).
    primary_read_only: AtomicBool,
}

impl Shared {
    fn new(opts: LibraryOptions) -> Self {
        Self {
            data_dir: opts.data_dir,
            bus: opts.bus,
            recording: opts.recording,
            stopping: AtomicBool::new(false),
            storage: Mutex::new(sync::expand_storage(opts.storage)),
            ready: AtomicBool::new(false),
            import: Mutex::new(None),
            primary_read_only: AtomicBool::new(false),
        }
    }

    /// Copy of the storage settings in effect.
    pub(crate) fn storage(&self) -> Storage {
        lock(&self.storage).clone()
    }

    /// True once the encoder probe has finished.
    pub(crate) fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// True while a recording session is being written.
    pub(crate) fn recording(&self) -> bool {
        self.recording.load(Ordering::SeqCst)
    }

    /// True once the service has been asked to stop.
    pub(crate) fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// True when the primary root is read-only.
    pub(crate) fn primary_read_only(&self) -> bool {
        self.primary_read_only.load(Ordering::SeqCst)
    }

    /// Records whether the primary root is read-only.
    pub(crate) fn set_primary_read_only(&self, value: bool) {
        self.primary_read_only.store(value, Ordering::SeqCst);
    }
}

/// Handle to the library service. Cheap to clone; every clone talks to the same worker.
#[derive(Clone)]
pub struct LibraryService {
    inner: Arc<Inner>,
}

struct Inner {
    tx: Sender<worker::Cmd>,
    join: Mutex<Option<JoinHandle<()>>>,
    shared: Arc<Shared>,
}

impl LibraryService {
    /// Opens the library for the worker and starts the `library` thread. The first root sync
    /// runs on that thread. Scanning waits until [`LibraryService::set_ready`] was called.
    pub fn start(opts: LibraryOptions) -> Result<LibraryService, String> {
        let library = Library::open(opts.data_dir.join(LIBRARY_FILE))
            .map_err(|e| format!("cannot open the library for scanning: {e}"))?;
        let shared = Arc::new(Shared::new(opts));
        let (tx, rx) = mpsc::channel::<worker::Cmd>();
        let worker_tx = tx.clone();
        let worker_shared = Arc::clone(&shared);
        let join = thread::Builder::new()
            .name("library".to_owned())
            .spawn(move || worker::run(library, rx, worker_tx, worker_shared))
            .map_err(|e| format!("cannot start the library thread: {e}"))?;
        Ok(LibraryService {
            inner: Arc::new(Inner {
                tx,
                join: Mutex::new(Some(join)),
                shared,
            }),
        })
    }

    /// Stops the worker and waits for it. Running scans end after their current step and
    /// a running import is cancelled. Safe to call more than once.
    pub fn stop(&self) {
        self.inner.shared.stopping.store(true, Ordering::SeqCst);
        if let Some(slot) = lock(&self.inner.shared.import).as_ref() {
            slot.cancel.store(true, Ordering::SeqCst);
        }
        let _ = self.inner.tx.send(worker::Cmd::Stop);
        let handle = lock(&self.inner.join).take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    /// Tells the service that the encoder probe has finished. Scans and thumbnails
    /// start once this is called.
    pub fn set_ready(&self) {
        self.inner.shared.ready.store(true, Ordering::SeqCst);
    }

    /// Tells the service that the storage settings changed. Roots are reconciled and
    /// new roots are scanned.
    pub fn storage_changed(&self, storage: Storage) {
        *lock(&self.inner.shared.storage) = sync::expand_storage(storage);
        let _ = self.inner.tx.send(worker::Cmd::StorageChanged);
    }

    /// True once the encoder probe has finished.
    pub(crate) fn is_ready(&self) -> bool {
        self.inner.shared.is_ready()
    }

    /// Daemon data directory, which holds `library.db` and `thumbs`. IPC handlers open their own
    /// connection to `library.db` from it (SQLite serialises the writers).
    pub(crate) fn data_dir(&self) -> PathBuf {
        self.inner.shared.data_dir.clone()
    }

    /// Answers one library IPC command. Commands that are not library commands get
    /// `not_implemented`.
    pub fn handle(&self, command: Command) -> Reply {
        match command {
            Command::ImportPlan { paths, game } => self.import_plan(&paths, game.as_deref()),
            Command::ImportStart { paths, game } => self.import_start(paths, game),
            Command::ImportCancel { job_id } => self.import_cancel(&job_id),
            other => self.ask_worker(other),
        }
    }

    /// Sends a command to the worker and waits for its reply.
    fn ask_worker(&self, command: Command) -> Reply {
        let (reply_tx, reply_rx) = mpsc::channel();
        if self
            .inner
            .tx
            .send(worker::Cmd::Ipc(command, reply_tx))
            .is_err()
        {
            return error(
                ErrorCode::ShuttingDown,
                "the library service has stopped",
                false,
            );
        }
        match reply_rx.recv_timeout(IPC_TIMEOUT) {
            Ok(reply) => reply,
            Err(RecvTimeoutError::Timeout) => {
                error(ErrorCode::Busy, "the library did not answer in time", true)
            }
            Err(RecvTimeoutError::Disconnected) => error(
                ErrorCode::ShuttingDown,
                "the library service has stopped",
                false,
            ),
        }
    }

    /// Plans an import. Walks the sources and checks free space; copies nothing.
    fn import_plan(&self, paths: &[String], game: Option<&str>) -> Reply {
        if paths.is_empty() {
            return error(
                ErrorCode::BadRequest,
                "no files or folders were given",
                false,
            );
        }
        let shared = &self.inner.shared;
        let root = shared.storage().root;
        let plan = import::plan_request(&root, paths, game);
        Reply::ImportPlan {
            items: plan.items.iter().map(import::item_dto).collect(),
            total_bytes: plan.total_bytes,
            free_bytes: plan.free_bytes,
            enough_space: plan.enough_space,
            primary_read_only: shared.primary_read_only(),
        }
    }

    /// Starts an import job. Refuses when one is already running, when the primary root is
    /// read-only, when the free space is short or when there is nothing to copy.
    fn import_start(&self, paths: Vec<String>, game: Option<String>) -> Reply {
        if paths.is_empty() {
            return error(
                ErrorCode::BadRequest,
                "no files or folders were given",
                false,
            );
        }
        let shared = &self.inner.shared;
        let job_id = Ulid::new().to_string();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut slot = lock(&shared.import);
            if slot.is_some() {
                return error(ErrorCode::Busy, "another import is already running", true);
            }
            // Reserved before planning, so a second request sees the job as busy.
            *slot = Some(ImportSlot {
                job_id: job_id.clone(),
                cancel: Arc::clone(&cancel),
            });
        }
        match self.start_job(job_id.clone(), cancel, &paths, game.as_deref()) {
            Ok(()) => Reply::ImportStarted { job_id },
            Err(reply) => {
                *lock(&shared.import) = None;
                reply
            }
        }
    }

    /// Checks the preconditions and starts the import thread. Returns the refusal as a reply.
    fn start_job(
        &self,
        job_id: String,
        cancel: Arc<AtomicBool>,
        paths: &[String],
        game: Option<&str>,
    ) -> Result<(), Reply> {
        let shared = &self.inner.shared;
        if !shared.is_ready() {
            return Err(error(
                ErrorCode::WorkerUnavailable,
                "The recorder is still starting, so imports are unavailable",
                true,
            ));
        }
        let storage = shared.storage();
        if shared.primary_read_only() {
            return Err(error(
                ErrorCode::InvalidState,
                "the primary library folder is read-only, so nothing can be imported into it",
                false,
            ));
        }
        let plan = import::plan_request(&storage.root, paths, game);
        if !plan.items.iter().any(|item| item.skip_reason.is_none()) {
            return Err(error(
                ErrorCode::BadRequest,
                "no new supported video files to import",
                false,
            ));
        }
        if !plan.enough_space {
            return Err(error(
                ErrorCode::InvalidState,
                format!(
                    "not enough free space in the primary library folder: the copy needs {} bytes plus a 1 GiB margin, {} bytes are free",
                    plan.total_bytes, plan.free_bytes
                ),
                false,
            ));
        }
        let spec = import::JobSpec {
            job_id,
            plan,
            root: PathBuf::from(sync::normalize(&storage.root.to_string_lossy())),
            data_dir: shared.data_dir.clone(),
            bus: Arc::clone(&shared.bus),
            cancel,
        };
        let release = Arc::clone(shared);
        thread::Builder::new()
            .name("library-import".to_owned())
            .spawn(move || {
                import::run_job(spec, move || {
                    *lock(&release.import) = None;
                });
            })
            .map(|_| ())
            .map_err(|e| {
                error(
                    ErrorCode::Internal,
                    format!("cannot start the import thread: {e}"),
                    false,
                )
            })
    }

    /// Cancels the running import when `job_id` names it.
    fn import_cancel(&self, job_id: &str) -> Reply {
        let slot = lock(&self.inner.shared.import);
        match slot.as_ref() {
            Some(running) if running.job_id == job_id => {
                running.cancel.store(true, Ordering::SeqCst);
                Reply::Ok
            }
            _ => error(
                ErrorCode::InvalidState,
                "no running import has that job id",
                false,
            ),
        }
    }
}

/// Builds an error reply.
pub(crate) fn error(code: ErrorCode, message: impl Into<String>, retryable: bool) -> Reply {
    Reply::Error {
        code,
        message: message.into(),
        retryable,
    }
}

/// Rate limiter for events: `ready` is true at most once per interval.
#[derive(Debug)]
pub(crate) struct Throttle {
    every: Duration,
    last: Option<Instant>,
}

impl Throttle {
    /// A throttle that allows one event every `every_ms` milliseconds.
    pub(crate) fn new(every_ms: u64) -> Self {
        Self {
            every: Duration::from_millis(every_ms),
            last: None,
        }
    }

    /// True when an event may be sent now. Calling it with true starts a new interval.
    pub(crate) fn ready(&mut self) -> bool {
        let now = Instant::now();
        match self.last {
            Some(last) if now.duration_since(last) < self.every => false,
            _ => {
                self.last = Some(now);
                true
            }
        }
    }
}
