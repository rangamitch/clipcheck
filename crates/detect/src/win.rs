//! Windows snapshot provider for game detection.
//!
//! Enumerates top-level windows and produces plain-data [`CandidateSnapshot`]s
//! for the pure detection logic. This module only reads window metadata and
//! the image path of the owning process (via `PROCESS_QUERY_LIMITED_INFORMATION`).
//! It never reads or writes foreign process memory and never injects code.
//!
//! [`enumerate_candidates`] returns raw window metadata only. The fields below
//! are left neutral here and are filled by [`crate::enrich::Enricher`], which
//! [`crate::runner::WinSource`] applies on every poll:
//! - `gpu_3d_load_pct`, `gpu_3d_avg60_pct`, `loaded_graphics_modules`,
//!   `in_known_db`, `in_library_scan`, `ms_since_foreground`.
//!
//! Other neutral defaults set here:
//! - `is_launcher`: `false` (`score::evaluate_with` checks the launcher list).
//! - `is_primary_in_group`: `true` (no process-tree grouping yet).
//! - `is_running_game`: `false` (the debouncer owns the running state).
//!
//! `exe_name` is empty when the owning process cannot be opened (for example an
//! elevated game while this process is not elevated).

use std::collections::HashMap;
use std::ffi::c_void;
use std::mem::size_of;

use windows::core::{HRESULT, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, BOOL, ERROR_INVALID_PARAMETER, FILETIME, HANDLE, HWND, LPARAM, RECT,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject,
    PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, GWL_EXSTYLE,
    GWL_STYLE, WS_CAPTION, WS_EX_TOOLWINDOW,
};

use crate::{CandidateSnapshot, CaptureTarget, Rect};

/// Enumerates all top-level windows that are visible or minimized and returns
/// one [`CandidateSnapshot`] per window, in z-order (topmost first).
///
/// Never panics on individual window failures: a window whose attributes cannot
/// be read gets default values for those fields.
pub fn enumerate_candidates() -> Vec<CandidateSnapshot> {
    enumerate_candidates_with(&mut ProcessCache::default())
}

/// Same as [`enumerate_candidates`], but reuses `procs` across polls so that the
/// exe name of a process is queried only when the process is new.
pub fn enumerate_candidates_with(procs: &mut ProcessCache) -> Vec<CandidateSnapshot> {
    procs.begin_poll();
    let mut ctx = EnumCtx {
        out: Vec::new(),
        procs,
    };
    let ptr = &mut ctx as *mut EnumCtx<'_>;
    // SAFETY: `EnumWindows` invokes `enum_proc` synchronously on this thread
    // before returning, so the pointer to `ctx` is valid for the whole call and
    // no other reference to `ctx` is alive during it.
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(ptr as isize));
    }
    let EnumCtx { out, procs } = ctx;
    procs.end_poll();
    out
}

/// State shared with `enum_proc` for one enumeration.
struct EnumCtx<'a> {
    out: Vec<CandidateSnapshot>,
    procs: &'a mut ProcessCache,
}

/// Per-process facts that stay fixed while the process lives, keyed by pid.
///
/// An entry is reused only when the process creation time still matches, so a
/// reused pid is never given the name of an earlier process. Entries are dropped
/// at the end of a poll when their pid did not own a listed window.
#[derive(Debug, Default)]
pub struct ProcessCache {
    entries: HashMap<u32, CachedProcess>,
    /// Number of the current poll; bumped by `begin_poll`.
    poll: u64,
    /// Current time as FILETIME, read once per poll.
    now_filetime: u64,
}

#[derive(Debug, Clone)]
struct CachedProcess {
    /// Process creation time as FILETIME; zero when it could not be read.
    created: u64,
    /// File name of the process image.
    exe: String,
    /// Poll in which this pid was last listed.
    last_poll: u64,
}

impl ProcessCache {
    /// Starts a new poll: bumps the poll number and reads the clock once.
    fn begin_poll(&mut self) {
        self.poll += 1;
        // SAFETY: no arguments; returns the current system time.
        self.now_filetime = filetime_to_u64(&unsafe { GetSystemTimeAsFileTime() });
    }

    /// Drops every pid that was not listed in the current poll.
    fn end_poll(&mut self) {
        let poll = self.poll;
        self.entries.retain(|_, e| e.last_poll == poll);
    }

    /// Returns `(exe_name, process_age_ms)` for `pid`.
    ///
    /// The first call for a pid in a poll validates the entry with one process
    /// handle (creation time) and queries the image name only on a miss. Later
    /// calls in the same poll return the stored values. On failure (for example
    /// an elevated process) the name is empty and the age is 0.
    fn info(&mut self, pid: u32) -> (String, u64) {
        if pid == 0 {
            return (String::new(), 0);
        }
        if let Some(entry) = self.entries.get(&pid) {
            if entry.last_poll == self.poll {
                return (entry.exe.clone(), age_ms(entry.created, self.now_filetime));
            }
        }

        let Some(handle) = open_process(pid) else {
            self.entries.insert(
                pid,
                CachedProcess {
                    created: 0,
                    exe: String::new(),
                    last_poll: self.poll,
                },
            );
            return (String::new(), 0);
        };

        let created = process_creation(handle).unwrap_or(0);
        let exe = match self.entries.get(&pid) {
            Some(old) if created != 0 && old.created == created => old.exe.clone(),
            _ => query_image_name(handle).unwrap_or_default(),
        };
        close_handle(handle);

        let age = if created == 0 {
            0
        } else {
            age_ms(created, self.now_filetime)
        };
        self.entries.insert(
            pid,
            CachedProcess {
                created,
                exe: exe.clone(),
                last_poll: self.poll,
            },
        );
        (exe, age)
    }
}

/// Milliseconds from `created` to `now` (both FILETIME values).
fn age_ms(created: u64, now: u64) -> u64 {
    now.saturating_sub(created) / 10_000
}

/// Returns the current foreground window as an opaque handle value, or `None`
/// if no window is foreground.
pub fn foreground_hwnd() -> Option<u64> {
    // SAFETY: `GetForegroundWindow` takes no arguments and only returns a handle value.
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        None
    } else {
        Some(hwnd_to_u64(hwnd))
    }
}

/// Callback for `EnumWindows`. Must not panic or unwind across the FFI boundary.
///
/// # Safety
/// Called by the OS with a valid `HWND`. `lparam` must be the pointer to the
/// `EnumCtx` set up in [`enumerate_candidates_with`], which is alive for the
/// duration of the enumeration.
unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` was produced from a live `&mut EnumCtx` in
    // `enumerate_candidates_with`, and enumeration is synchronous on this thread,
    // so this is the only mutable access to the context.
    let ctx = unsafe { &mut *(lparam.0 as *mut EnumCtx<'_>) };

    // SAFETY: `hwnd` is a handle supplied by the OS during enumeration.
    let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
    // SAFETY: as above.
    let minimized = unsafe { IsIconic(hwnd) }.as_bool();
    if !visible && !minimized {
        return BOOL(1);
    }

    if let Some(snap) = snapshot_window(hwnd, visible, minimized, ctx.procs) {
        ctx.out.push(snap);
    }
    BOOL(1)
}

/// Builds the snapshot for one window. Returns `None` only when the window has
/// no usable geometry (it is then skipped).
fn snapshot_window(
    hwnd: HWND,
    visible: bool,
    minimized: bool,
    procs: &mut ProcessCache,
) -> Option<CandidateSnapshot> {
    // SAFETY: `hwnd` is a live window handle from the enumeration callback; the
    // out-parameter is a valid, initialized `RECT` on our stack.
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return None;
    }
    let window_rect = rect_from(&rect);

    // SAFETY: `DwmGetWindowAttribute` writes a `u32` into the buffer we pass,
    // whose size is given as `size_of::<u32>()`.
    let is_cloaked = {
        let mut cloaked: u32 = 0;
        let res = unsafe {
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked as *mut u32 as *mut c_void,
                size_of::<u32>() as u32,
            )
        };
        res.is_ok() && cloaked != 0
    };

    // SAFETY: plain read of the extended style for a valid window handle.
    let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let is_tool_window = (ex_style as usize) & (WS_EX_TOOLWINDOW.0 as usize) != 0;

    // SAFETY: plain read of the window style for a valid window handle.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) };
    let has_caption = (style as usize) & (WS_CAPTION.0 as usize) == WS_CAPTION.0 as usize;

    let mut pid: u32 = 0;
    // SAFETY: the optional out-pointer is a valid `*mut u32` on our stack.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }

    let title = window_title(hwnd);
    let monitor_rect = monitor_rect_for(hwnd);
    let (exe_name, process_age_ms) = procs.info(pid);

    Some(CandidateSnapshot {
        exe_name,
        window_title: title,
        window_rect,
        monitor_rect,
        target: CaptureTarget {
            pid,
            hwnd: hwnd_to_u64(hwnd),
        },
        is_visible: visible,
        is_minimized: minimized,
        is_cloaked,
        is_tool_window,
        has_caption,
        is_launcher: false,
        is_primary_in_group: true,
        in_known_db: false,
        in_library_scan: false,
        gpu_3d_load_pct: None,
        gpu_3d_avg60_pct: None,
        loaded_graphics_modules: Vec::new(),
        ms_since_foreground: None,
        is_running_game: false,
        process_age_ms,
    })
}

/// Reads the window title, or an empty string if unavailable.
fn window_title(hwnd: HWND) -> String {
    // SAFETY: valid window handle; returns the title length in characters.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return String::new();
    }
    let mut buf = vec![0u16; len as usize + 1];
    // SAFETY: `buf` is a writable slice; the API writes at most `buf.len()` UTF-16 units.
    let written = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if written <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..written as usize])
}

/// Returns the rectangle of the monitor nearest to the window, or a zero rect
/// if the monitor cannot be queried.
fn monitor_rect_for(hwnd: HWND) -> Rect {
    // SAFETY: valid window handle; returns a monitor handle (possibly null).
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.0.is_null() {
        return Rect::from_size(0, 0, 0, 0);
    }
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info.cbSize` is set to the struct size as the API requires.
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return Rect::from_size(0, 0, 0, 0);
    }
    rect_from(&info.rcMonitor)
}

/// Opens `pid` with `PROCESS_QUERY_LIMITED_INFORMATION` only. `None` when the
/// process cannot be opened (for example an elevated process).
fn open_process(pid: u32) -> Option<HANDLE> {
    // SAFETY: requests only limited query rights; the caller closes the handle.
    unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()
}

/// Why a [`ProcessWatch`] could not be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchError {
    /// The pid names no process any more.
    Gone,
    /// The process exists but the watch right was refused, or the call failed otherwise.
    Denied,
}

/// A handle with `SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION` on one process.
///
/// The handle keeps the process object alive, so the OS cannot hand the pid to
/// another process while the watch exists. [`ProcessWatch::is_running`] therefore
/// cannot be fooled by pid reuse. The watch grants no read, write or memory access.
#[derive(Debug)]
pub struct ProcessWatch {
    pid: u32,
    /// The raw `HANDLE` value, stored as an integer so the watch is `Send`.
    handle: isize,
}

impl ProcessWatch {
    /// Opens a watch on `pid`.
    pub fn open(pid: u32) -> Result<Self, WatchError> {
        let access = PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;
        // SAFETY: requests only the synchronize and limited-query rights. The
        // returned handle is owned by the watch and closed exactly once in `Drop`.
        match unsafe { OpenProcess(access, false, pid) } {
            Ok(handle) => Ok(Self {
                pid,
                handle: handle.0 as isize,
            }),
            Err(e) if e.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
                Err(WatchError::Gone)
            }
            Err(_) => Err(WatchError::Denied),
        }
    }

    /// The watched pid.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// `Some(true)` while the process runs, `Some(false)` once it has exited, and
    /// `None` when the wait itself fails. Never blocks.
    pub fn is_running(&self) -> Option<bool> {
        // SAFETY: the handle stays valid until `Drop`. A zero timeout only polls.
        match unsafe { WaitForSingleObject(self.raw(), 0) } {
            WAIT_TIMEOUT => Some(true),
            WAIT_OBJECT_0 => Some(false),
            _ => None,
        }
    }

    fn raw(&self) -> HANDLE {
        HANDLE(self.handle as *mut c_void)
    }
}

impl Drop for ProcessWatch {
    fn drop(&mut self) {
        close_handle(self.raw());
    }
}

/// True while `hwnd` is a live window that is visible or minimized, the same test
/// the enumeration applies. One cheap call for the liveness probe.
pub fn window_alive(hwnd: u64) -> bool {
    let h = HWND(hwnd as usize as *mut c_void);
    // SAFETY: these window queries accept any handle value and only read state.
    unsafe { IsWindow(h).as_bool() && (IsWindowVisible(h).as_bool() || IsIconic(h).as_bool()) }
}

/// Closes a handle returned by [`open_process`].
fn close_handle(handle: HANDLE) {
    // SAFETY: `handle` was returned by a successful `OpenProcess` and is closed exactly once.
    unsafe {
        let _ = CloseHandle(handle);
    }
}

/// Returns the file name (e.g. `Game.exe`) of the process image.
fn query_image_name(handle: HANDLE) -> Option<String> {
    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `buf` is writable for `len` UTF-16 units and `len` is updated by the API.
    unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        )
        .ok()?;
    }
    let full = String::from_utf16_lossy(&buf[..len as usize]);
    let file = full.rsplit(['\\', '/']).next().unwrap_or("").to_string();
    Some(file)
}

/// Process creation time as FILETIME (100 ns units since 1601).
fn process_creation(handle: HANDLE) -> Option<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all four out-parameters are valid `FILETIME`s on our stack.
    unsafe {
        GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user).ok()?;
    }
    Some(filetime_to_u64(&creation))
}

fn filetime_to_u64(ft: &FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

fn rect_from(r: &RECT) -> Rect {
    Rect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

fn hwnd_to_u64(hwnd: HWND) -> u64 {
    hwnd.0 as usize as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_combines_halves() {
        let ft = FILETIME {
            dwLowDateTime: 5,
            dwHighDateTime: 1,
        };
        assert_eq!(filetime_to_u64(&ft), (1u64 << 32) | 5);
    }

    fn entry(created: u64, exe: &str, last_poll: u64) -> CachedProcess {
        CachedProcess {
            created,
            exe: exe.into(),
            last_poll,
        }
    }

    #[test]
    fn same_poll_hit_returns_stored_name_and_fresh_age() {
        let mut cache = ProcessCache::default();
        cache.begin_poll();
        // Created 5 000 ms before the poll clock, listed in this poll already.
        let created = cache.now_filetime - 5_000 * 10_000;
        cache
            .entries
            .insert(1234, entry(created, "game.exe", cache.poll));
        // Pid 1234 must not be opened: the entry answers on its own.
        assert_eq!(cache.info(1234), ("game.exe".to_string(), 5_000));
    }

    #[test]
    fn pid_zero_is_never_cached_or_probed() {
        let mut cache = ProcessCache::default();
        cache.begin_poll();
        assert_eq!(cache.info(0), (String::new(), 0));
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn end_poll_evicts_pids_not_listed_in_this_poll() {
        let mut cache = ProcessCache::default();
        cache.begin_poll();
        cache.entries.insert(1, entry(7, "a.exe", cache.poll));
        cache.entries.insert(2, entry(8, "b.exe", cache.poll - 1));
        cache.end_poll();
        assert!(cache.entries.contains_key(&1));
        assert!(!cache.entries.contains_key(&2));
    }

    #[test]
    fn age_is_zero_for_unknown_creation_and_saturates() {
        assert_eq!(age_ms(0, 0), 0);
        assert_eq!(age_ms(100, 50), 0);
        assert_eq!(age_ms(0, 20_000), 2);
    }
}
