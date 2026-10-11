// Prevents a console window on Windows in every profile, debug included.
// The daemon writes its own log file, so it needs no console either.
#![cfg_attr(windows, windows_subsystem = "windows")]

use clipcheck_ui_lib::{cli, daemon};

fn main() {
    // `ClipCheck.exe --daemon ...` runs the recorder with no window and never returns here.
    if let Some(code) = daemon::maybe_run_daemon(std::env::args().collect()) {
        std::process::exit(code);
    }

    // `ClipCheck.exe --shutdown-daemon` (used by the installer) stops only this exe's daemon.
    let args: Vec<String> = std::env::args().collect();
    if let Some(code) = cli::maybe_run_cli(&args) {
        std::process::exit(code);
    }

    // A normal launch makes sure a detached daemon records in the background, so closing
    // the window does not stop recording. Runs on its own thread so the window is not delayed.
    std::thread::spawn(|| {
        if let Err(e) = daemon::ensure_daemon_running(None) {
            eprintln!("clipcheck-ui: {e}");
        }
    });

    clipcheck_ui_lib::run()
}
