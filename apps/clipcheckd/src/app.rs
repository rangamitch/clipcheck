//! Daemon startup and shutdown sequence, argument parsing and shared state.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clipcheck_core::config::schema::Config;
use clipcheck_core::config::validate::Severity;
use clipcheck_ipc::proto::{AutostartState, Event, StatusDto};
use clipcheck_ipc::{default_pipe_name, PipeServer};
use clipcheck_library::Library;

use crate::configstore::ConfigStore;
use crate::configwatch::ConfigWatcher;
use crate::events::EventBus;
use crate::instance::{self, SingleInstance, SingleInstanceError};
use crate::logging;
use crate::server;
use crate::service::{Service, ServiceError, ServiceOptions};
use crate::tray::{self, Tray, TrayHandlers};

/// How often the tray tooltip follows the service status. The tray applies
/// updates no faster than [`tray::TOOLTIP_MIN_INTERVAL`], so polling faster
/// only costs service round trips.
const TOOLTIP_POLL: Duration = tray::TOOLTIP_MIN_INTERVAL;
/// Time given to IPC writer threads to send the final events before the process exits.
const EXIT_FLUSH: Duration = Duration::from_millis(200);

/// Exit code: clean shutdown.
pub const EXIT_OK: i32 = 0;
/// Exit code: generic startup failure.
pub const EXIT_FAILURE: i32 = 1;
/// Exit code: the IPC pipe name is owned by another user or process.
pub const EXIT_PIPE_OWNED: i32 = 2;
/// Exit code: another daemon already runs in this session.
pub const EXIT_ALREADY_RUNNING: i32 = 3;
/// Exit code: bad command-line usage.
pub const EXIT_USAGE: i32 = 64;

/// Config file name inside the data directory.
pub const CONFIG_FILE: &str = "config.toml";
/// Library database file name inside the data directory.
pub const LIBRARY_FILE: &str = "library.db";

const USAGE: &str = "usage: clipcheckd [--data-dir <path>] [--pipe-name <name>]";

/// Parsed command-line arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Args {
    /// Directory holding config, library, and logs.
    pub data_dir: PathBuf,
    /// Named pipe the IPC server listens on.
    pub pipe_name: String,
    /// Hidden test flag: start in degraded mode as if no encoder were found.
    pub no_encoders: bool,
}

/// Parses `--data-dir` and `--pipe-name`. Defaults: `%APPDATA%\ClipCheck` and
/// the per-user pipe name from [`clipcheck_ipc::default_pipe_name`].
///
/// `--no-encoders` is a hidden flag for tests. It starts the daemon in degraded
/// mode without running the encoder probe. `--no-ffmpeg` is a hidden compatibility alias of
/// `--no-encoders`, kept for older test scripts.
pub fn parse_args<I>(args: I) -> Result<Args, String>
where
    I: IntoIterator<Item = String>,
{
    let mut data_dir = None;
    let mut pipe_name = None;
    let mut no_encoders = false;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--data-dir" => {
                let value = iter.next().ok_or("--data-dir needs a path")?;
                data_dir = Some(PathBuf::from(value));
            }
            "--pipe-name" => {
                let value = iter.next().ok_or("--pipe-name needs a name")?;
                pipe_name = Some(value);
            }
            // `--no-ffmpeg` is a hidden compatibility alias of `--no-encoders`.
            "--no-encoders" | "--no-ffmpeg" => no_encoders = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let data_dir = match data_dir {
        Some(dir) => dir,
        None => std::env::var_os("APPDATA")
            .map(|appdata| PathBuf::from(appdata).join("ClipCheck"))
            .ok_or("APPDATA is not set; pass --data-dir")?,
    };
    let pipe_name = match pipe_name {
        Some(name) => name,
        None => default_pipe_name().map_err(|e| format!("cannot build default pipe name: {e}"))?,
    };
    Ok(Args {
        data_dir,
        pipe_name,
        no_encoders,
    })
}

/// A one-shot, clonable shutdown signal.
#[derive(Debug, Clone, Default)]
pub struct ShutdownToken(Arc<(Mutex<bool>, Condvar)>);

impl ShutdownToken {
    /// Creates an untriggered token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks shutdown as requested and wakes every waiter. Idempotent.
    pub fn trigger(&self) {
        let (flag, cvar) = &*self.0;
        let mut guard = flag.lock().unwrap_or_else(|e| e.into_inner());
        *guard = true;
        cvar.notify_all();
    }

    /// Blocks the calling thread until [`ShutdownToken::trigger`] is called.
    pub fn wait(&self) {
        let (flag, cvar) = &*self.0;
        let mut guard = flag.lock().unwrap_or_else(|e| e.into_inner());
        while !*guard {
            guard = cvar.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
    }
}

/// Token signalled by the console control handler.
static SHUTDOWN: OnceLock<ShutdownToken> = OnceLock::new();

/// Starts the graceful shutdown path used by Ctrl+C and the `shutdown` command.
/// The watcher thread logs, then exits the process with [`EXIT_OK`].
pub(crate) fn request_shutdown() {
    if let Some(token) = SHUTDOWN.get() {
        token.trigger();
    }
}

/// State shared by the accept loop and every connection thread.
pub struct AppState {
    started: Instant,
    /// Configuration in effect: the config loaded at startup. Later edits to
    /// `config.toml` are picked up by the file watcher (`configwatch`), which
    /// applies them through the service.
    pub config: Config,
    /// Pushed IPC events.
    pub bus: Arc<EventBus>,
    /// The recording service (recorder, detection, hotkeys, library writes).
    pub service: Service,
}

impl AppState {
    /// Builds the shared state. Uptime is measured from this call.
    pub fn new(config: Config, bus: Arc<EventBus>, service: Service) -> Self {
        Self {
            started: Instant::now(),
            config,
            bus,
            service,
        }
    }

    /// Snapshot for `get_status`: the service's view plus the daemon-level fields.
    pub fn status(&self) -> Result<StatusDto, ServiceError> {
        let mut status = self.service.status()?;
        status.autostart = if self.config.general.autostart {
            AutostartState::Enabled
        } else {
            AutostartState::Disabled
        };
        status.uptime_s = self.started.elapsed().as_secs();
        status.exe_path = std::env::current_exe()
            .ok()
            .map(|path| path.display().to_string());
        Ok(status)
    }
}

/// Runs the daemon with the given arguments (without the program name) and
/// returns the process exit code.
pub fn run<I>(raw_args: I) -> i32
where
    I: IntoIterator<Item = String>,
{
    let args = match parse_args(raw_args) {
        Ok(args) => args,
        Err(msg) => {
            eprintln!("clipcheckd: {msg}\n{USAGE}");
            return EXIT_USAGE;
        }
    };

    // The mutex comes first so a second instance never touches the first one's logs or files.
    let _instance = match SingleInstance::acquire(&instance::mutex_name_for(&args.data_dir)) {
        Ok(instance) => instance,
        Err(SingleInstanceError::AlreadyRunning) => {
            eprintln!("clipcheckd: another instance is already running in this session");
            return EXIT_ALREADY_RUNNING;
        }
        Err(e) => {
            eprintln!("clipcheckd: {e}");
            return EXIT_FAILURE;
        }
    };

    if let Err(e) = fs::create_dir_all(&args.data_dir) {
        eprintln!(
            "clipcheckd: cannot create data dir {}: {e}",
            args.data_dir.display()
        );
        return EXIT_FAILURE;
    }
    if let Err(e) = logging::init(&args.data_dir.join("logs")) {
        eprintln!("clipcheckd: cannot open log file: {e}");
        return EXIT_FAILURE;
    }
    install_panic_hook();
    logging::info(&format!(
        "clipcheckd {} starting (pid {}, data dir {}, pipe {})",
        env!("CARGO_PKG_VERSION"),
        process::id(),
        args.data_dir.display(),
        args.pipe_name
    ));

    let config = load_config(&args.data_dir.join(CONFIG_FILE));

    let library = match Library::open(args.data_dir.join(LIBRARY_FILE)) {
        Ok(library) => library,
        Err(e) => {
            logging::error(&format!("cannot open library database: {e}"));
            return EXIT_FAILURE;
        }
    };

    let shutdown = ShutdownToken::new();
    let _ = SHUTDOWN.set(shutdown.clone());
    if let Err(e) = instance::install_ctrl_handler(request_shutdown) {
        // Not fatal: the daemon still runs, it just cannot catch Ctrl+C.
        logging::warn(&format!("cannot install console control handler: {e}"));
    }

    let server = match PipeServer::bind(&args.pipe_name) {
        Ok(server) => server,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            logging::error(&format!(
                "pipe {} is owned by another user or process: {e}",
                args.pipe_name
            ));
            return EXIT_PIPE_OWNED;
        }
        Err(e) => {
            logging::error(&format!("cannot bind pipe {}: {e}", args.pipe_name));
            return EXIT_FAILURE;
        }
    };

    // The pipe server is not Send (it holds a raw security descriptor), so it
    // stays on this thread. A watcher thread turns the shutdown signal into
    // process exit, which also ends the blocked accept call.
    let library = Arc::new(Mutex::new(library));
    let bus = Arc::new(EventBus::new());
    let service = match Service::start(ServiceOptions {
        config: config.clone(),
        data_dir: args.data_dir.clone(),
        library: Arc::clone(&library),
        bus: Arc::clone(&bus),
        no_encoders: args.no_encoders,
    }) {
        Ok(service) => service,
        Err(e) => {
            logging::error(&e);
            return EXIT_FAILURE;
        }
    };
    let state = Arc::new(AppState::new(config, bus, service));

    // Hand edits to config.toml take effect without a restart. The watcher
    // lives until `run` returns; failing to start it is not fatal.
    let _config_watch =
        match ConfigWatcher::start(args.data_dir.join(CONFIG_FILE), state.service.clone()) {
            Ok(watch) => Some(watch),
            Err(e) => {
                logging::warn(&format!("cannot watch config.toml: {e}"));
                None
            }
        };

    // The tray keeps the daemon visible and reachable when no UI window is open.
    let open_handler: Box<dyn Fn() + Send + Sync> = Box::new(tray::launch_ui);
    let exit_service = state.service.clone();
    let exit_handler: Box<dyn Fn() + Send + Sync> = Box::new(move || {
        if tray::exit_confirmed(&exit_service, tray::confirm_exit) {
            logging::info("exit requested from the tray");
            request_shutdown();
        } else {
            logging::info("exit from the tray cancelled");
        }
    });
    let tray = Tray::spawn(TrayHandlers {
        open_ui: open_handler,
        request_exit: exit_handler,
    });
    spawn_tooltip_updater(tray.clone(), state.service.clone());

    let watcher_state = Arc::clone(&state);
    let watcher_tray = tray.clone();
    let watcher = thread::Builder::new()
        .name("shutdown-watch".to_owned())
        .spawn(move || {
            shutdown.wait();
            logging::info("shutdown requested; stopping recording service");
            // Every exit path (tray Exit, the shutdown command, Ctrl+C) ends here.
            // Open UIs learn the process is leaving, then the icon goes.
            watcher_state.bus.publish(Event::AppExit);
            thread::sleep(EXIT_FLUSH);
            watcher_tray.shutdown();
            watcher_state.service.stop();
            logging::info("shutdown requested; exiting");
            // process::exit skips destructors, so buffered lines are written here.
            logging::flush();
            process::exit(EXIT_OK);
        });
    if let Err(e) = watcher {
        logging::error(&format!("cannot start shutdown watcher: {e}"));
        return EXIT_FAILURE;
    }
    logging::info(&format!("listening on {}", args.pipe_name));

    server::serve(server, state);
    logging::flush();
    EXIT_OK
}

/// Keeps the tray tooltip in step with the service: game and session time while recording.
fn spawn_tooltip_updater(tray: tray::TrayHandle, service: Service) {
    let spawned = thread::Builder::new()
        .name("tray-tooltip".to_owned())
        .spawn(move || loop {
            thread::sleep(TOOLTIP_POLL);
            if let Ok(status) = service.status() {
                tray.set_tooltip(&tray::tooltip_for_status(&status, now_unix_ms()));
            }
        });
    if let Err(e) = spawned {
        logging::warn(&format!("cannot start the tray tooltip updater: {e}"));
    }
}

/// Loads `config.toml`. Writes the default file when missing. Any read,
/// parse, or validation error logs a warning and falls back to defaults; the
/// bad file is left untouched so the user can fix it. A later save copies it to
/// `config.toml.broken-<unix_ms>` before writing (see `ConfigStore::save`).
pub fn load_config(path: &Path) -> Config {
    if !path.exists() {
        match fs::write(path, Config::default_toml()) {
            Ok(()) => logging::info(&format!("wrote default config to {}", path.display())),
            Err(e) => logging::warn(&format!("cannot write default config: {e}")),
        }
        return Config::default();
    }

    // The same read and validation path that reload uses (ConfigStore::load_checked).
    let (config, diagnostics) = ConfigStore::new(path.to_path_buf()).load_checked();
    for d in &diagnostics {
        let msg = if d.path.is_empty() {
            format!("config {}: {}", d.code, d.message)
        } else {
            format!("config {} {}: {}", d.code, d.path, d.message)
        };
        match d.severity {
            Severity::Error => config_warning(&msg),
            Severity::Warning => logging::warn(&msg),
        }
    }
    match config {
        Some(config) => config,
        None => {
            config_warning("config has errors, using defaults");
            Config::default()
        }
    }
}

/// Logs a config problem and echoes it to stderr, since the daemon may be
/// started by hand and the user should see why defaults are in use.
fn config_warning(msg: &str) {
    logging::warn(msg);
    eprintln!("clipcheckd: warning: {msg}");
}

/// Routes panics through the file logger before the default hook runs.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        logging::error(&format!("panic: {info}"));
        default_hook(info);
    }));
}

/// Current time in Unix milliseconds, for `pong`.
pub fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
