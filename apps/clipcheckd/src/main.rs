//! `clipcheckd.exe`: thin entry point. The daemon itself lives in the
//! library crate so `ClipCheck.exe --daemon` can run it too.
//!
//! Windows GUI subsystem: it starts at logon (`--background`), so it must never open a
//! console window. Its diagnostics go to stderr and the log file, which the subsystem
//! change does not affect.

#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    std::process::exit(clipcheckd::run());
}
