//! `clipcheckd`: the always-running ClipCheck recorder daemon.
//!
//! The daemon ships as `clipcheckd.exe` (this crate's binary) and also runs
//! inside `ClipCheck.exe` via `ClipCheck.exe --daemon`. This library holds the
//! whole daemon so both entry points share one implementation.
//!
//! Startup, shutdown, and the request loop live in [`app`] and [`server`].

pub mod app;
pub mod appinfo;
pub mod autostart;
pub mod branding;
pub mod configstore;
pub mod configwatch;
pub mod discovery;
pub mod events;
pub mod gameicons;
pub mod instance;
pub mod library_service;
pub mod logging;
pub mod salvage;
pub mod server;
pub mod service;
pub mod settings_map;
pub mod sound;
pub mod toast;
pub mod tray;

/// Runs the daemon with the process arguments and returns the exit code.
///
/// Equivalent to `run_with_args(std::env::args().collect())`.
pub fn run() -> i32 {
    run_with_args(std::env::args().collect())
}

/// Runs the daemon. `args` is the full argv as the OS passed it: the first
/// element is the program name and is skipped. Returns the process exit code.
pub fn run_with_args(args: Vec<String>) -> i32 {
    app::run(args.into_iter().skip(1))
}
