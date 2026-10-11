//! Windows named-pipe transport for the clipcheck IPC protocol.
//!
//! Overlapped (asynchronous) I/O: every pipe handle is created with
//! `FILE_FLAG_OVERLAPPED`, and each read, write and connect uses its own
//! `OVERLAPPED`. Read and write events are pooled per [`PipeConn`] and reused
//! across operations instead of being created for every call. The Windows I/O
//! manager serializes synchronous I/O per file object, so a synchronous read would block a write issued through a
//! duplicated handle. With overlapped I/O, a [`PipeConn`] can be cloned with
//! [`PipeConn::try_clone`] and one clone can sit in a blocking read while
//! another writes. The pipe is byte-mode, rejects remote (SMB) clients, and is
//! protected by a DACL that grants access only to the current user and SYSTEM
//! (design doc 10, section 3.4.1).
//!
//! `unsafe` is confined to this module; every block carries a `// SAFETY:`
//! comment.

use std::ffi::c_void;
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use windows::core::{Error as WinError, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    LocalFree, BOOL, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_HANDLE_EOF, ERROR_IO_PENDING,
    ERROR_NOT_FOUND, ERROR_OPERATION_ABORTED, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ,
    GENERIC_WRITE, HANDLE, HLOCAL,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenUser, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY,
    TOKEN_USER,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_FIRST_PIPE_INSTANCE,
    FILE_FLAG_OVERLAPPED, FILE_SHARE_MODE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeW, NAMED_PIPE_MODE, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    CreateEventW, GetCurrentProcess, OpenProcessToken, ResetEvent,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

use crate::MAX_CLIENTS;

/// Pipe in/out buffer size in bytes (design doc 10, section 3.4.1).
const PIPE_BUFFER_BYTES: u32 = 65536;

/// Pause between attempts when the server end of the pipe does not exist yet.
const NOT_FOUND_RETRY: Duration = Duration::from_millis(20);

/// Builds the default per-user pipe name `\\.\pipe\clipcheck-ipc-<user-sid>`
/// (design doc 10, D12). The SID comes from the current process token.
pub fn default_pipe_name() -> io::Result<String> {
    Ok(format!(r"\\.\pipe\clipcheck-ipc-{}", current_user_sid()?))
}

/// Listening side of a named pipe.
///
/// Holds one pending pipe instance at all times so that a client can always
/// connect without waiting for the server loop to create a new instance.
#[derive(Debug)]
pub struct PipeServer {
    name: String,
    name_w: Vec<u16>,
    sd: SecurityDescriptor,
    next: Option<OwnedHandle>,
}

impl PipeServer {
    /// Creates the first pipe instance under `name`.
    ///
    /// Uses `FILE_FLAG_FIRST_PIPE_INSTANCE`, so this fails with
    /// `PermissionDenied` (ERROR_ACCESS_DENIED) when any instance of that name
    /// already exists, including one owned by another process.
    pub fn bind(name: &str) -> io::Result<Self> {
        let name_w = to_wide(name);
        let sd = SecurityDescriptor::for_current_user()?;
        let first = create_instance(&name_w, &sd, true)?;
        Ok(Self {
            name: name.to_owned(),
            name_w,
            sd,
            next: Some(first),
        })
    }

    /// Returns the pipe name this server listens on.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Blocks until a client connects and returns the connection.
    ///
    /// A fresh pending instance is created before returning, so the next call
    /// can accept immediately. If that creation fails, the connection is still
    /// returned and the instance is retried on the following call.
    pub fn accept(&mut self) -> io::Result<PipeConn> {
        let instance = match self.next.take() {
            Some(handle) => handle,
            None => create_instance(&self.name_w, &self.sd, false)?,
        };

        let raw = HANDLE(instance.as_raw_handle());
        let event = create_event()?;
        let mut ov = OVERLAPPED {
            hEvent: HANDLE(event.as_raw_handle()),
            ..Default::default()
        };
        // SAFETY: `raw` is a valid overlapped pipe instance owned by `instance`,
        // and `ov` with its event live until the wait below completes.
        let started = unsafe { ConnectNamedPipe(raw, Some(&mut ov)) };
        match started {
            // ERROR_PIPE_CONNECTED means the client connected between create
            // and connect. No operation is pending, so there is nothing to wait for.
            Err(e) if win32_code(&e) == ERROR_PIPE_CONNECTED.0 => {}
            Err(e) if win32_code(&e) != ERROR_IO_PENDING.0 => return Err(to_io(e)),
            _ => {
                wait_result(raw, &ov)?;
            }
        }

        self.next = create_instance(&self.name_w, &self.sd, false).ok();
        Ok(PipeConn {
            shared: Arc::new(Shared::new(instance)),
        })
    }
}

/// Client-side connector for a named pipe.
#[derive(Debug, Clone, Copy)]
pub struct PipeClient;

impl PipeClient {
    /// Connects to the server at `name`, retrying until `timeout` elapses.
    ///
    /// Waits for a busy server instance (ERROR_PIPE_BUSY) and polls for a
    /// server that has not created its instance yet (ERROR_FILE_NOT_FOUND).
    pub fn connect(name: &str, timeout: Duration) -> io::Result<PipeConn> {
        let name_w = to_wide(name);
        let deadline = Instant::now() + timeout;
        loop {
            // SAFETY: `name_w` is a NUL-terminated UTF-16 buffer that outlives
            // the call. The returned handle is wrapped in OwnedHandle right
            // away, so it is closed exactly once.
            let opened = unsafe {
                CreateFileW(
                    PCWSTR(name_w.as_ptr()),
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    None,
                )
            };
            match opened {
                Ok(handle) => {
                    // SAFETY: CreateFileW returned a fresh handle that nothing
                    // else owns, so OwnedHandle takes sole ownership of it.
                    let owned = unsafe { OwnedHandle::from_raw_handle(handle.0) };
                    return Ok(PipeConn {
                        shared: Arc::new(Shared::new(owned)),
                    });
                }
                Err(e) => {
                    let code = win32_code(&e);
                    let retryable = code == ERROR_PIPE_BUSY.0 || code == ERROR_FILE_NOT_FOUND.0;
                    if !retryable {
                        return Err(to_io(e));
                    }
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("pipe connect timed out (win32 error {code})"),
                        ));
                    }
                    let remaining = u32::try_from((deadline - now).as_millis()).unwrap_or(u32::MAX);
                    if code == ERROR_PIPE_BUSY.0 {
                        // SAFETY: `name_w` is a valid NUL-terminated buffer for
                        // the duration of the call. The result is ignored
                        // because the loop re-checks the deadline and retries.
                        let _ = unsafe { WaitNamedPipeW(PCWSTR(name_w.as_ptr()), remaining) };
                    } else {
                        thread::sleep(NOT_FOUND_RETRY);
                    }
                }
            }
        }
    }
}

/// One connected pipe. Implements [`Read`] and [`Write`] for use with
/// [`crate::frame::read_frame`] and [`crate::frame::write_frame`].
///
/// Clones made with [`PipeConn::try_clone`] share the same pipe instance. Each
/// read or write runs as its own overlapped operation, so one clone can block
/// in a read while another clone writes on a different thread.
#[derive(Debug)]
pub struct PipeConn {
    shared: Arc<Shared>,
}

/// State shared by every clone of one pipe instance.
#[derive(Debug)]
struct Shared {
    handle: OwnedHandle,
    read_events: EventPool,
    write_events: EventPool,
    /// Set by [`PipeConn::cancel_pending`]. Once set, reads fail at once.
    cancelled: AtomicBool,
}

/// Idle manual-reset events for one direction of a pipe.
///
/// An operation takes an event out of the pool and returns it when it finishes,
/// so sequential reads (or writes) reuse one event. Concurrent operations on
/// different clones each take their own event, so no event is ever in use by
/// two operations at once.
#[derive(Debug, Default)]
struct EventPool(Mutex<Vec<OwnedHandle>>);

impl Shared {
    fn new(handle: OwnedHandle) -> Self {
        Self {
            handle,
            read_events: EventPool::default(),
            write_events: EventPool::default(),
            cancelled: AtomicBool::new(false),
        }
    }
}

impl EventPool {
    fn take(&self) -> io::Result<OwnedHandle> {
        let idle = self.idle().pop();
        match idle {
            Some(event) => Ok(event),
            None => create_event(),
        }
    }

    fn give_back(&self, event: OwnedHandle) {
        self.idle().push(event);
    }

    fn idle(&self) -> MutexGuard<'_, Vec<OwnedHandle>> {
        // The pool holds only handles, so a poisoned lock is still consistent.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl PipeConn {
    /// Returns another handle to the same pipe instance.
    ///
    /// Reads on one clone and writes on another may run at the same time, on
    /// different threads. The pipe closes once every clone is dropped.
    pub fn try_clone(&self) -> io::Result<PipeConn> {
        Ok(PipeConn {
            shared: Arc::clone(&self.shared),
        })
    }

    /// Cancels reads on this pipe instance and unblocks any read in progress.
    ///
    /// Design note: the cancel is sticky. The flag is stored on the shared
    /// pipe instance before `CancelIoEx` runs, so a read that has not yet
    /// issued its `ReadFile` also fails, and a read that issues it just before
    /// the cancel is cancelled by its own thread. Without the flag a cancel
    /// that arrives early would be lost and the reader would block forever.
    ///
    /// Behavior:
    /// - Every clone and every thread sharing this instance is affected. A
    ///   read blocked elsewhere returns `ERROR_OPERATION_ABORTED` (raw OS
    ///   error 995). That is the same error for an in-flight read and for a
    ///   read started after the cancel. It is not `ErrorKind::Interrupted`,
    ///   because `read_frame` retries on that kind and would loop forever.
    /// - A read that completed with data before the cancel returns that data.
    ///   Nothing is lost. The next read fails.
    /// - Overlapped writes pending at the moment of the call are cancelled
    ///   too (`CancelIoEx` with no OVERLAPPED pointer). Writes issued later
    ///   are not affected by the flag. Call this only when the connection is
    ///   being closed.
    /// - Nothing pending (`ERROR_NOT_FOUND`) and repeated calls succeed.
    pub fn cancel_pending(&self) -> io::Result<()> {
        self.shared.cancelled.store(true, Ordering::SeqCst);
        // SAFETY: `raw()` is a valid handle kept alive by `self.shared`. A None
        // OVERLAPPED pointer cancels all pending operations on the handle.
        match unsafe { CancelIoEx(self.raw(), None) } {
            Ok(()) => Ok(()),
            Err(e) if win32_code(&e) == ERROR_NOT_FOUND.0 => Ok(()),
            Err(e) => Err(to_io(e)),
        }
    }

    fn raw(&self) -> HANDLE {
        HANDLE(self.shared.handle.as_raw_handle())
    }
}

impl Read for PipeConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.shared.cancelled.load(Ordering::SeqCst) {
            return Err(cancelled_error());
        }
        let raw = self.raw();
        let result = overlapped(
            raw,
            &self.shared.read_events,
            Some(&self.shared.cancelled),
            |ov| {
                // SAFETY: `raw` is a valid overlapped handle, `buf` is a live
                // buffer for the whole operation (overlapped() waits for it to
                // finish before returning), and `ov` points at the OVERLAPPED
                // that overlapped() keeps alive for the same period.
                unsafe { ReadFile(raw, Some(&mut *buf), None, Some(ov)) }
            },
        );
        match result {
            Ok(n) => Ok(n),
            // Peer closed the pipe: report end of stream, as std does for pipes.
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(code) if code == ERROR_BROKEN_PIPE.0 as i32 || code == ERROR_HANDLE_EOF.0 as i32
                ) =>
            {
                Ok(0)
            }
            Err(e) => Err(e),
        }
    }
}

impl Write for PipeConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let raw = self.raw();
        overlapped(raw, &self.shared.write_events, None, |ov| {
            // SAFETY: as in `read`: `buf` and `ov` stay alive until the
            // operation completes, because overlapped() waits before returning.
            unsafe { WriteFile(raw, Some(buf), None, Some(ov)) }
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        // Pipe writes go straight to the kernel buffer; there is nothing to flush.
        Ok(())
    }
}

/// Runs one overlapped read or write and blocks until it completes.
///
/// The operation uses an event taken from `events` and returns it afterwards.
/// `start` receives a pointer to an `OVERLAPPED` bound to that event. When
/// `cancelled` is given, the operation is cancelled by its own thread if the
/// flag is already set once it has been issued (see [`PipeConn::cancel_pending`]).
fn overlapped<F>(
    handle: HANDLE,
    events: &EventPool,
    cancelled: Option<&AtomicBool>,
    start: F,
) -> io::Result<usize>
where
    F: FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
{
    let event = events.take()?;
    let result = run_overlapped(handle, &event, cancelled, start);
    events.give_back(event);
    result
}

/// Resets `event`, starts one operation bound to it, and waits for completion.
///
/// An `ERROR_IO_PENDING` result means the operation is in flight, and the wait
/// below completes it. Any other error is returned at once. A call that finishes
/// synchronously is also waited on, which returns immediately.
///
/// Race handling: the flag is checked after the operation is issued. If a cancel
/// stored the flag before this check, this thread cancels its own operation.
/// If the flag was stored later, the cancel's `CancelIoEx` sees the issued
/// operation. A `CancelIoEx` that finds nothing pending is harmless and fails
/// with `ERROR_NOT_FOUND`, which is ignored.
fn run_overlapped<F>(
    handle: HANDLE,
    event: &OwnedHandle,
    cancelled: Option<&AtomicBool>,
    start: F,
) -> io::Result<usize>
where
    F: FnOnce(*mut OVERLAPPED) -> windows::core::Result<()>,
{
    let event_raw = HANDLE(event.as_raw_handle());
    // SAFETY: `event_raw` is a valid event handle owned by `event`, which is
    // alive for the call. Clearing the signaled state here means a completion
    // left over from an earlier operation cannot satisfy this wait.
    unsafe { ResetEvent(event_raw) }.map_err(to_io)?;
    let mut ov = OVERLAPPED {
        hEvent: event_raw,
        ..Default::default()
    };
    match start(&mut ov as *mut OVERLAPPED) {
        Ok(()) => {}
        Err(e) if win32_code(&e) == ERROR_IO_PENDING.0 => {}
        Err(e) => return Err(to_io(e)),
    }
    if cancelled.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
        cancel_operation(handle, &ov);
    }
    wait_result(handle, &ov)
}

/// Cancels one operation issued on `handle` with `ov`. The result is ignored:
/// `ERROR_NOT_FOUND` means the operation already finished, and the wait that
/// follows reports the real outcome, including data that arrived first.
fn cancel_operation(handle: HANDLE, ov: &OVERLAPPED) {
    // SAFETY: `handle` is a valid overlapped handle and `ov` is the OVERLAPPED
    // of an operation issued on it. It stays alive until the caller's wait
    // returns, which is after this call.
    let _ = unsafe { CancelIoEx(handle, Some(ov as *const OVERLAPPED)) };
}

/// The error returned by a read on a cancelled pipe instance: the same raw OS
/// error (`ERROR_OPERATION_ABORTED`) as a read that was in flight when cancelled.
fn cancelled_error() -> io::Error {
    io::Error::from_raw_os_error(ERROR_OPERATION_ABORTED.0 as i32)
}

/// Blocks until the overlapped operation described by `ov` finishes, and returns
/// the number of bytes it transferred.
fn wait_result(handle: HANDLE, ov: &OVERLAPPED) -> io::Result<usize> {
    let mut transferred = 0u32;
    // SAFETY: `handle` is a valid overlapped handle and `ov` is the structure
    // of the operation started on it; both are alive for the call. With bWait
    // set, the call returns only after the operation completes, so the kernel
    // no longer touches `ov` or the caller's buffer afterwards.
    unsafe { GetOverlappedResult(handle, ov, &mut transferred, true) }.map_err(to_io)?;
    Ok(transferred as usize)
}

/// Creates a manual-reset, unnamed event for one overlapped operation.
fn create_event() -> io::Result<OwnedHandle> {
    // SAFETY: no security attributes and no name are passed, so the call only
    // allocates a new event object. The returned handle is wrapped in
    // OwnedHandle right away, so it is closed exactly once.
    let handle = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(to_io)?;
    // SAFETY: CreateEventW returned a fresh handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
}

/// Owns a `PSECURITY_DESCRIPTOR` allocated by the SDDL conversion.
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

impl std::fmt::Debug for SecurityDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecurityDescriptor")
    }
}

impl SecurityDescriptor {
    /// Builds the protected DACL `D:P(A;;GA;;;<user SID>)(A;;GA;;;SY)`.
    fn for_current_user() -> io::Result<Self> {
        let sid = current_user_sid()?;
        let sddl = to_wide(&format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"));
        let mut psd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` is a NUL-terminated buffer that outlives the call and
        // `psd` is a valid out-pointer. On success the system allocates the
        // descriptor, which SecurityDescriptor frees in Drop.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(sddl.as_ptr()),
                SDDL_REVISION_1,
                &mut psd,
                None,
            )
        }
        .map_err(to_io)?;
        Ok(Self(psd))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: the pointer came from ConvertStringSecurityDescriptor...,
        // which allocates with LocalAlloc, so LocalFree is the matching release.
        // It is freed exactly once because Drop runs once per owner.
        unsafe {
            let _ = LocalFree(HLOCAL(self.0 .0));
        }
    }
}

/// Creates one pipe instance. Only the first instance of a server passes
/// `first = true`, which adds FILE_FLAG_FIRST_PIPE_INSTANCE.
fn create_instance(name: &[u16], sd: &SecurityDescriptor, first: bool) -> io::Result<OwnedHandle> {
    let mut open_mode = PIPE_ACCESS_DUPLEX.0 | FILE_FLAG_OVERLAPPED.0;
    if first {
        open_mode |= FILE_FLAG_FIRST_PIPE_INSTANCE.0;
    }
    let pipe_mode = NAMED_PIPE_MODE(
        PIPE_TYPE_BYTE.0 | PIPE_READMODE_BYTE.0 | PIPE_WAIT.0 | PIPE_REJECT_REMOTE_CLIENTS.0,
    );
    let sa = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0 .0,
        bInheritHandle: BOOL(0),
    };
    let max_instances = u32::from(MAX_CLIENTS) + 1;
    // SAFETY: `name` is a NUL-terminated buffer and `sa` points at a live
    // SECURITY_ATTRIBUTES whose descriptor `sd` outlives the call. The
    // returned handle is checked below and then owned by OwnedHandle.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(name.as_ptr()),
            FILE_FLAGS_AND_ATTRIBUTES(open_mode),
            pipe_mode,
            max_instances,
            PIPE_BUFFER_BYTES,
            PIPE_BUFFER_BYTES,
            0,
            Some(&sa as *const SECURITY_ATTRIBUTES),
        )
    };
    if handle.is_invalid() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateNamedPipeW returned a valid handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle.0) })
}

/// Reads the current process user SID as a string, e.g. `S-1-5-21-...`.
fn current_user_sid() -> io::Result<String> {
    let mut token = HANDLE::default();
    // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no closing.
    // `token` is a valid out-pointer for the call.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.map_err(to_io)?;
    // SAFETY: OpenProcessToken succeeded, so `token` is a fresh handle that
    // only this OwnedHandle will close.
    let token = unsafe { OwnedHandle::from_raw_handle(token.0) };
    let token_handle = HANDLE(token.as_raw_handle());

    let mut needed = 0u32;
    // SAFETY: a call with no buffer only reports the required size. Its error
    // (ERROR_INSUFFICIENT_BUFFER) is expected and deliberately ignored.
    let _ = unsafe { GetTokenInformation(token_handle, TokenUser, None, 0, &mut needed) };

    // Vec<u64> keeps the buffer aligned for TOKEN_USER.
    let mut buf = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `buf` holds at least `needed` bytes and is 8-byte aligned, which
    // is enough for TOKEN_USER. `needed` is passed as the length.
    unsafe {
        GetTokenInformation(
            token_handle,
            TokenUser,
            Some(buf.as_mut_ptr().cast::<c_void>()),
            needed,
            &mut needed,
        )
    }
    .map_err(to_io)?;

    // SAFETY: GetTokenInformation(TokenUser) filled `buf` with a TOKEN_USER
    // whose SID points into the same buffer, so the reference is valid while
    // `buf` lives.
    let user = unsafe { &*(buf.as_ptr() as *const TOKEN_USER) };
    let mut sid_pwstr = PWSTR::null();
    // SAFETY: `user.User.Sid` is a valid SID from the token buffer, and
    // `sid_pwstr` is a valid out-pointer. On success it holds a LocalAlloc'd
    // string that is freed below.
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_pwstr) }.map_err(to_io)?;

    // SAFETY: on success `sid_pwstr` points at a NUL-terminated UTF-16 string.
    let text =
        unsafe { sid_pwstr.to_string() }.map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
    // SAFETY: `sid_pwstr` came from ConvertSidToStringSidW, which allocates with
    // LocalAlloc, and it is freed once here after the string was copied out.
    unsafe {
        let _ = LocalFree(HLOCAL(sid_pwstr.0.cast::<c_void>()));
    }
    text
}

/// Encodes `s` as NUL-terminated UTF-16.
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Extracts the Win32 error code from a windows-crate error.
fn win32_code(e: &WinError) -> u32 {
    let code = e.code().0 as u32;
    if code & 0xFFFF_0000 == 0x8007_0000 {
        code & 0xFFFF
    } else {
        code
    }
}

/// Converts a windows-crate error into an io::Error with the same OS code.
fn to_io(e: WinError) -> io::Error {
    let code = win32_code(&e);
    if code <= i32::MAX as u32 {
        io::Error::from_raw_os_error(code as i32)
    } else {
        io::Error::other(e)
    }
}
