//! Process-level Windows glue: the per-session single-instance mutex and the
//! console control handler (Ctrl+C, Ctrl+Break, close, logoff, shutdown).
//!
//! This is one of the two modules allowed to contain `unsafe` code. Every
//! block carries a `// SAFETY:` comment.

use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GetLastError, BOOL, ERROR_ALREADY_EXISTS};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Threading::CreateMutexW;

/// Session-local mutex name that makes the daemon a single instance per user session.
pub const MUTEX_NAME: &str = r"Local\ClipCheck.Recorder";

/// Lock name for one data folder. The daemon is single-instance PER DATA FOLDER: two daemons
/// must never write the same config/library, but unrelated folders (tests, a second profile)
/// must not block each other. The same folder always hashes to the same name.
pub fn mutex_name_for(data_dir: &std::path::Path) -> String {
    let normalized = data_dir
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase();
    // FNV-1a 64-bit: stable across runs and Rust versions (unlike `DefaultHasher`).
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in normalized.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{MUTEX_NAME}.{hash:016x}")
}

#[cfg(test)]
mod name_tests {
    use super::mutex_name_for;
    use std::path::Path;

    #[test]
    fn same_folder_same_name_regardless_of_case_slashes_and_trailing_separator() {
        let a = mutex_name_for(Path::new(r"C:\Users\Me\AppData\Roaming\ClipCheck"));
        assert_eq!(
            a,
            mutex_name_for(Path::new(r"c:\users\me\appdata\roaming\clipcheck\"))
        );
        assert_eq!(
            a,
            mutex_name_for(Path::new("C:/Users/Me/AppData/Roaming/ClipCheck"))
        );
    }

    #[test]
    fn different_folders_get_different_names() {
        assert_ne!(
            mutex_name_for(Path::new(r"C:\a")),
            mutex_name_for(Path::new(r"C:\b"))
        );
    }

    #[test]
    fn name_stays_session_local() {
        assert!(mutex_name_for(Path::new(r"C:\a")).starts_with(r"Local\ClipCheck.Recorder."));
    }
}

/// Errors from [`SingleInstance::acquire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SingleInstanceError {
    /// Another daemon already holds the mutex in this session.
    AlreadyRunning,
    /// Creating the mutex failed for another reason. Carries the Win32 error code.
    Win32(u32),
}

impl std::fmt::Display for SingleInstanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SingleInstanceError::AlreadyRunning => {
                write!(f, "another clipcheckd is already running in this session")
            }
            SingleInstanceError::Win32(code) => {
                write!(f, "cannot create instance mutex (win32 error {code})")
            }
        }
    }
}

/// Holds the single-instance mutex. The mutex is released when this value drops
/// (or when the process exits).
#[derive(Debug)]
pub struct SingleInstance {
    _handle: OwnedHandle,
}

impl SingleInstance {
    /// Creates or opens the named mutex. Returns [`SingleInstanceError::AlreadyRunning`]
    /// when the mutex already existed.
    pub fn acquire(name: &str) -> Result<Self, SingleInstanceError> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is NUL-terminated and outlives the call. Null security
        // attributes give the default DACL, and the mutex is not initially owned.
        let created = unsafe { CreateMutexW(None, BOOL(0), PCWSTR(wide.as_ptr())) };
        let handle = match created {
            Ok(handle) => handle,
            Err(_) => {
                // SAFETY: GetLastError only reads the calling thread's last-error value.
                let code = unsafe { GetLastError() }.0;
                return Err(SingleInstanceError::Win32(code));
            }
        };
        // SAFETY: GetLastError reads the last-error value set by CreateMutexW
        // on this thread. ERROR_ALREADY_EXISTS means the mutex was opened, not created.
        let last = unsafe { GetLastError() };
        // SAFETY: `handle` is a fresh, valid handle returned by CreateMutexW that
        // nothing else owns, so OwnedHandle takes sole ownership and closes it once.
        let owned = unsafe { OwnedHandle::from_raw_handle(handle.0) };
        if last == ERROR_ALREADY_EXISTS {
            // Dropping `owned` closes our reference; the first owner keeps the mutex.
            return Err(SingleInstanceError::AlreadyRunning);
        }
        Ok(Self { _handle: owned })
    }
}

/// Callback run when a console control event arrives. Set once by [`install_ctrl_handler`].
static CTRL_CALLBACK: OnceLock<fn()> = OnceLock::new();

/// Installs a console control handler that calls `on_event` for Ctrl+C,
/// Ctrl+Break, console close, logoff and system shutdown.
///
/// The callback runs on a Windows-created thread, so it must be short and must not block.
pub fn install_ctrl_handler(on_event: fn()) -> windows::core::Result<()> {
    let _ = CTRL_CALLBACK.set(on_event);
    // SAFETY: `ctrl_handler` has the signature SetConsoleCtrlHandler requires and
    // has static lifetime, so the handler stays valid for the whole process.
    unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), BOOL(1)) }
}

/// Handler invoked by the console subsystem. Returning TRUE tells Windows the
/// event was handled, so the default process termination does not run.
unsafe extern "system" fn ctrl_handler(_ctrl_type: u32) -> BOOL {
    if let Some(callback) = CTRL_CALLBACK.get() {
        callback();
    }
    BOOL(1)
}
